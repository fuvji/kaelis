//! Install a vanilla client from Mojang's version manifest. Downloads are
//! checksum-verified and written under the instance root before metadata makes
//! the version visible to the launcher.

use super::downloader::{DownloadError, DownloadTask, download_all_with_progress};
use super::versions::VersionEntry;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use thiserror::Error;

const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;
const DOWNLOAD_CONCURRENCY: usize = 12;
static METADATA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum VanillaInstallError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("download failed: {0}")]
    Download(#[from] DownloadError),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON metadata is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("metadata is missing {0}")]
    Missing(&'static str),
    #[error("metadata is invalid: {0}")]
    Invalid(&'static str),
    #[error("unsafe path in Mojang metadata: {0}")]
    UnsafePath(String),
    #[error("Minecraft metadata version {metadata} did not match requested version {requested}")]
    VersionMismatch { metadata: String, requested: String },
    #[error("blocking install task failed: {0}")]
    Join(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallProgress {
    Resolving,
    DownloadingLibraries { completed: usize, total: usize },
    DownloadingAssetIndex,
    DownloadingAssets { completed: usize, total: usize },
    DownloadingClient,
    MaterializingAssets,
    Finalizing,
    Complete,
}

impl InstallProgress {
    pub fn message(self) -> String {
        match self {
            Self::Resolving => "Loading Mojang version metadata…".to_owned(),
            Self::DownloadingLibraries { completed, total } => {
                format!("Downloading libraries ({completed}/{total})…")
            }
            Self::DownloadingAssetIndex => "Downloading asset index…".to_owned(),
            Self::DownloadingAssets { completed, total } => {
                format!("Downloading game assets ({completed}/{total})…")
            }
            Self::DownloadingClient => "Downloading Minecraft client…".to_owned(),
            Self::MaterializingAssets => "Preparing legacy game assets…".to_owned(),
            Self::Finalizing => "Finalizing vanilla installation…".to_owned(),
            Self::Complete => "Vanilla installation complete.".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AssetIndex {
    #[serde(default)]
    objects: HashMap<String, AssetObject>,
    #[serde(default, rename = "virtual")]
    is_virtual: bool,
    #[serde(default)]
    map_to_resources: bool,
}

#[derive(Debug, Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

pub(super) fn valid_version_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn safe_relative_path(value: &str) -> Result<PathBuf, VanillaInstallError> {
    if value.is_empty() || value.contains(['\\', '\0', ':']) || value.starts_with('/') {
        return Err(VanillaInstallError::UnsafePath(value.to_owned()));
    }
    let mut result = PathBuf::new();
    for segment in value.split('/') {
        if segment.is_empty() || matches!(segment, "." | "..") {
            return Err(VanillaInstallError::UnsafePath(value.to_owned()));
        }
        result.push(segment);
    }
    if result
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(VanillaInstallError::UnsafePath(value.to_owned()));
    }
    Ok(result)
}

pub(super) fn safe_destination(
    root: &Path,
    relative: &str,
) -> Result<PathBuf, VanillaInstallError> {
    let relative = safe_relative_path(relative)?;
    let absolute_root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut current = PathBuf::new();
    for component in absolute_root.components() {
        match component {
            Component::Prefix(prefix) => {
                current.push(prefix.as_os_str());
                continue;
            }
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(segment) => current.push(segment),
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(VanillaInstallError::UnsafePath(
                    absolute_root.display().to_string(),
                ));
            }
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(VanillaInstallError::UnsafePath(
                    current.display().to_string(),
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(VanillaInstallError::UnsafePath(
                    current.display().to_string(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(create_error)
                        if create_error.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        let metadata = std::fs::symlink_metadata(&current)?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(VanillaInstallError::UnsafePath(
                                current.display().to_string(),
                            ));
                        }
                    }
                    Err(create_error) => return Err(create_error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    let canonical_root = std::fs::canonicalize(root)?;
    let mut destination = canonical_root.clone();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(segment) = component else {
            return Err(VanillaInstallError::UnsafePath(
                relative.display().to_string(),
            ));
        };
        destination.push(segment);
        if index + 1 == components.len() {
            if std::fs::symlink_metadata(&destination)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(VanillaInstallError::UnsafePath(
                    destination.display().to_string(),
                ));
            }
            break;
        }
        match std::fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(VanillaInstallError::UnsafePath(
                    destination.display().to_string(),
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(VanillaInstallError::UnsafePath(
                    destination.display().to_string(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&destination)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if !destination.starts_with(canonical_root) {
        return Err(VanillaInstallError::UnsafePath(
            destination.display().to_string(),
        ));
    }
    Ok(destination)
}

async fn fetch_json(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, VanillaInstallError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| VanillaInstallError::Invalid("URL"))?;
    if parsed.scheme() != "https" {
        return Err(VanillaInstallError::Invalid("HTTPS metadata URL"));
    }
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(parsed.clone())
            .header("User-Agent", kaelis_core::brand::user_agent())
    })
    .await?
    .error_for_status()?;
    if response.url().scheme() != "https"
        || response
            .content_length()
            .is_some_and(|length| length > MAX_METADATA_BYTES as u64)
    {
        return Err(VanillaInstallError::Invalid(
            "metadata response size or URL",
        ));
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(VanillaInstallError::Invalid("metadata response size"));
    }
    Ok(bytes.to_vec())
}

fn metadata_download_task(
    info: &Value,
    download_root: &Path,
) -> Result<DownloadTask, VanillaInstallError> {
    let url = info
        .get("url")
        .and_then(Value::as_str)
        .ok_or(VanillaInstallError::Missing("download URL"))?;
    let path = info
        .get("path")
        .and_then(Value::as_str)
        .ok_or(VanillaInstallError::Missing("download path"))?;
    let destination = safe_destination(download_root, path)?;
    let sha1 = info.get("sha1").and_then(Value::as_str).unwrap_or_default();
    let mut task = DownloadTask::new(url, destination, sha1);
    if let Some(size) = info.get("size").and_then(Value::as_u64) {
        task = task.with_size(size);
    }
    Ok(task)
}

fn maven_path(coordinate: &str) -> Result<String, VanillaInstallError> {
    let (coordinate, extension) = coordinate
        .split_once('@')
        .map_or((coordinate, "jar"), |(name, extension)| (name, extension));
    if extension.is_empty()
        || !extension
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    {
        return Err(VanillaInstallError::Invalid("Maven extension"));
    }
    let parts: Vec<_> = coordinate.split(':').collect();
    if !(3..=4).contains(&parts.len()) || parts.iter().any(|part| part.is_empty()) {
        return Err(VanillaInstallError::Invalid("Maven coordinate"));
    }
    let group = parts[0].split('.').collect::<Vec<_>>();
    if group.iter().any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }) || parts[1..].iter().any(|part| {
        !part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }) {
        return Err(VanillaInstallError::Invalid("Maven coordinate"));
    }
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts
        .get(3)
        .map(|value| format!("-{value}"))
        .unwrap_or_default();
    Ok(format!(
        "{}/{}/{}/{artifact}-{version}{classifier}.{extension}",
        group.join("/"),
        artifact,
        version
    ))
}

fn maven_native_classifier(library: &Value) -> Option<&str> {
    library
        .get("name")
        .and_then(Value::as_str)
        .and_then(|name| name.split(':').nth(3))
        .filter(|classifier| classifier.starts_with("natives-"))
}

fn maven_native_classifier_matches(classifier: &str) -> bool {
    let os_name = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let Some(suffix) = classifier.strip_prefix(&format!("natives-{os_name}")) else {
        return false;
    };
    if suffix.is_empty() {
        return cfg!(target_arch = "x86_64");
    }
    let Some(arch) = suffix.strip_prefix('-') else {
        return false;
    };
    match std::env::consts::ARCH {
        "x86_64" => matches!(arch, "64" | "x64" | "x86_64" | "amd64"),
        "x86" => matches!(arch, "32" | "x86" | "i386"),
        "aarch64" => matches!(arch, "arm64" | "aarch64"),
        _ => false,
    }
}

fn library_allowed(library: &Value) -> bool {
    let Some(rules) = library.get("rules").and_then(Value::as_array) else {
        return maven_native_classifier(library).is_none_or(maven_native_classifier_matches);
    };
    let mut allowed = false;
    let os_name = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    };
    for rule in rules {
        let matches = rule
            .pointer("/os/name")
            .and_then(Value::as_str)
            .is_none_or(|name| name == os_name)
            && rule
                .pointer("/os/arch")
                .and_then(Value::as_str)
                .is_none_or(|arch| arch == std::env::consts::ARCH)
            && rule.get("features").is_none();
        if matches {
            allowed = rule
                .get("action")
                .and_then(Value::as_str)
                .is_some_and(|action| action == "allow");
        }
    }
    allowed && maven_native_classifier(library).is_none_or(maven_native_classifier_matches)
}

fn platform_arch() -> &'static str {
    if cfg!(target_arch = "x86") {
        "32"
    } else {
        "64"
    }
}

fn add_task(
    tasks: &mut Vec<DownloadTask>,
    destinations: &mut HashSet<PathBuf>,
    task: DownloadTask,
) -> Result<(), VanillaInstallError> {
    if destinations.insert(task.dest.clone()) {
        tasks.push(task);
        Ok(())
    } else if let Some(existing) = tasks.iter().find(|existing| existing.dest == task.dest)
        && existing.url == task.url
        && existing.sha1.eq_ignore_ascii_case(&task.sha1)
        && existing.expected_size == task.expected_size
    {
        Ok(())
    } else {
        Err(VanillaInstallError::Invalid(
            "duplicate library destination",
        ))
    }
}

fn library_tasks(metadata: &Value, root: &Path) -> Result<Vec<DownloadTask>, VanillaInstallError> {
    let libraries = metadata
        .get("libraries")
        .and_then(Value::as_array)
        .ok_or(VanillaInstallError::Missing("libraries"))?;
    let library_root = root.join("libraries");
    let mut tasks = Vec::new();
    let mut destinations = HashSet::new();
    for library in libraries.iter().filter(|library| library_allowed(library)) {
        let downloads = library.get("downloads");
        if let Some(artifact) = downloads.and_then(|value| value.get("artifact")) {
            add_task(
                &mut tasks,
                &mut destinations,
                metadata_download_task(artifact, &library_root)?,
            )?;
        } else if let Some(coordinate) = library.get("name").and_then(Value::as_str) {
            let path = maven_path(coordinate)?;
            let destination = safe_destination(&library_root, &path)?;
            let base = library
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("https://libraries.minecraft.net/");
            let base = reqwest::Url::parse(base)
                .map_err(|_| VanillaInstallError::Invalid("library base URL"))?;
            if base.scheme() != "https" {
                return Err(VanillaInstallError::Invalid("HTTPS library base URL"));
            }
            let url = base
                .join(&path)
                .map_err(|_| VanillaInstallError::Invalid("library URL"))?;
            let sha1 = library
                .pointer("/checksums/0")
                .and_then(Value::as_str)
                .unwrap_or_default();
            add_task(
                &mut tasks,
                &mut destinations,
                DownloadTask::new(url.as_str(), destination, sha1),
            )?;
        }

        let native_name = library
            .pointer(&format!(
                "/natives/{}",
                if cfg!(target_os = "windows") {
                    "windows"
                } else if cfg!(target_os = "macos") {
                    "osx"
                } else {
                    "linux"
                }
            ))
            .and_then(Value::as_str)
            .map(|name| name.replace("${arch}", platform_arch()));
        let classifier = native_name.as_ref().and_then(|native_name| {
            downloads
                .and_then(|value| value.get("classifiers"))
                .and_then(|value| value.get(native_name))
        });
        if let Some(classifier) = classifier {
            add_task(
                &mut tasks,
                &mut destinations,
                metadata_download_task(classifier, &library_root)?,
            )?;
        }
    }
    Ok(tasks)
}

fn asset_tasks(index: &AssetIndex, root: &Path) -> Result<Vec<DownloadTask>, VanillaInstallError> {
    let object_root = root.join("assets").join("objects");
    let mut tasks = Vec::with_capacity(index.objects.len());
    let mut destinations = HashSet::with_capacity(index.objects.len());
    for object in index.objects.values() {
        if object.hash.len() != 40 || !object.hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(VanillaInstallError::Invalid("asset SHA-1"));
        }
        let hash = object.hash.to_ascii_lowercase();
        // Mojang's object layout repeats the hash: the first two characters are
        // the folder and the whole 40-character hash is the file name.
        let relative = format!("{}/{}", &hash[..2], hash);
        let destination = safe_destination(&object_root, &relative)?;
        if !destinations.insert(destination.clone()) {
            continue;
        }
        let url = format!("https://resources.download.minecraft.net/{relative}");
        tasks.push(DownloadTask::new(&url, destination, &hash).with_size(object.size));
    }
    Ok(tasks)
}

/// Marker written after a completed install.
///
/// The deep asset pass costs one filesystem lookup per game asset — fast on a
/// warm cache, slow on a cold spinning disk. The marker records that the pass
/// already ran to completion, so an installed instance pays for it once and
/// only cheap checks after that.
const COMPLETE_MARKER: &str = ".kaelis-complete.json";

fn complete_marker_path(root: &Path, version_id: &str) -> PathBuf {
    root.join("versions").join(version_id).join(COMPLETE_MARKER)
}

/// Whether [`install_vanilla`] already completed for this version here.
pub fn install_complete(root: &Path, version_id: &str) -> bool {
    let Ok(bytes) = std::fs::read(complete_marker_path(root, version_id)) else {
        return false;
    };
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|stamp| stamp.get("id").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|id| id == version_id)
}

fn write_complete_marker(root: &Path, version_id: &str) -> Result<(), VanillaInstallError> {
    let stamp = serde_json::json!({
        "id": version_id,
        "at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0),
    });
    write_version_metadata(
        &complete_marker_path(root, version_id),
        &serde_json::to_vec(&stamp)?,
    )
}

/// Which files a launch needs but cannot see under `root`.
///
/// Cheap by design: the version JSON and client jar are two stats, the library
/// list is checked by path, and the asset objects are only walked on an
/// instance that has no completion marker (an import, or a manual copy). An
/// empty result means the instance is launchable without touching the network.
pub fn missing_install_files(
    root: &Path,
    version_id: &str,
) -> Result<Vec<PathBuf>, VanillaInstallError> {
    if !valid_version_id(version_id) {
        return Err(VanillaInstallError::Invalid("version id"));
    }
    let version_dir = root.join("versions").join(version_id);
    let metadata_path = version_dir.join(format!("{version_id}.json"));
    if !metadata_path.is_file() {
        return Ok(vec![metadata_path]);
    }
    let client_jar = version_dir.join(format!("{version_id}.jar"));
    if !client_jar.is_file() {
        return Ok(vec![client_jar]);
    }
    let metadata: Value = serde_json::from_slice(&std::fs::read(&metadata_path)?)?;
    let mut missing = Vec::new();
    for task in library_tasks(&metadata, root)? {
        if !task.dest.is_file() {
            missing.push(task.dest);
        }
    }
    let Some(index_info) = metadata.get("assetIndex") else {
        return Ok(missing);
    };
    let Some(index_id) = index_info.get("id").and_then(Value::as_str) else {
        return Ok(missing);
    };
    if !valid_version_id(index_id) {
        return Err(VanillaInstallError::Invalid("asset index id"));
    }
    let index_path = safe_destination(
        &root.join("assets").join("indexes"),
        &format!("{index_id}.json"),
    )?;
    if !index_path.is_file() {
        missing.push(index_path);
        return Ok(missing);
    }
    if install_complete(root, version_id) {
        return Ok(missing);
    }
    let index_bytes = std::fs::read(&index_path)?;
    if index_bytes.len() > MAX_METADATA_BYTES {
        return Err(VanillaInstallError::Invalid("asset index size"));
    }
    let index: AssetIndex = serde_json::from_slice(&index_bytes)?;
    for task in asset_tasks(&index, root)? {
        if !task.dest.is_file() {
            missing.push(task.dest);
        }
    }
    Ok(missing)
}

fn throttled_progress<F>(
    callback: Arc<F>,
    last_reported: Arc<AtomicUsize>,
    stage: fn(usize, usize) -> InstallProgress,
) -> impl Fn(usize, usize) + Send + Sync + 'static
where
    F: Fn(InstallProgress) + Send + Sync + 'static,
{
    move |completed, total| {
        let interval = (total / 100).max(1);
        let previous = last_reported.load(Ordering::Relaxed);
        if completed == 0 || completed == total || completed.saturating_sub(previous) >= interval {
            last_reported.store(completed, Ordering::Relaxed);
            callback(stage(completed, total));
        }
    }
}

fn materialize_legacy_assets(root: &Path, index: &AssetIndex) -> Result<(), VanillaInstallError> {
    if !index.is_virtual && !index.map_to_resources {
        return Ok(());
    }
    for (name, object) in &index.objects {
        let relative = safe_relative_path(name)?;
        let hash = object.hash.to_ascii_lowercase();
        let source = root
            .join("assets")
            .join("objects")
            .join(&hash[..2])
            .join(&hash);
        if !source.is_file() {
            return Err(VanillaInstallError::Missing("downloaded asset object"));
        }
        if index.is_virtual {
            let destination = safe_destination(&root.join("assets").join("virtual"), name)?;
            std::fs::copy(&source, destination)?;
        }
        if index.map_to_resources {
            let destination = safe_destination(&root.join("assets").join("resources"), name)?;
            std::fs::copy(&source, destination)?;
        }
        drop(relative);
    }
    Ok(())
}

struct StagedMetadata {
    path: PathBuf,
    committed: bool,
}

impl Drop for StagedMetadata {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn write_version_metadata(path: &Path, bytes: &[u8]) -> Result<(), VanillaInstallError> {
    let parent = path
        .parent()
        .ok_or(VanillaInstallError::Invalid("version metadata path"))?;
    std::fs::create_dir_all(parent)?;
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(VanillaInstallError::UnsafePath(path.display().to_string()));
    }
    let sequence = METADATA_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".version-{}-{sequence}.part", std::process::id()));
    let mut staged = StagedMetadata {
        path: temporary.clone(),
        committed: false,
    };
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temporary, path)?;
    staged.committed = true;
    Ok(())
}

/// Install official client, libraries and assets under an instance's game
/// directory. Progress callbacks are throttled to at most roughly 100 updates
/// per download phase.
pub async fn install_vanilla<F>(
    client: &reqwest::Client,
    version: VersionEntry,
    root: PathBuf,
    progress: F,
) -> Result<(), VanillaInstallError>
where
    F: Fn(InstallProgress) + Send + Sync + 'static,
{
    let progress = Arc::new(progress);
    progress(InstallProgress::Resolving);
    if !valid_version_id(&version.id) {
        return Err(VanillaInstallError::Invalid("version id"));
    }
    let version_url = reqwest::Url::parse(&version.url)
        .map_err(|_| VanillaInstallError::Invalid("version metadata URL"))?;
    if version_url.scheme() != "https" {
        return Err(VanillaInstallError::Invalid("HTTPS version metadata URL"));
    }
    std::fs::create_dir_all(&root)?;
    // A marker from a previous, now-broken install must not survive into the
    // new one; it is written again only after every file is in place.
    let _ = std::fs::remove_file(complete_marker_path(&root, &version.id));
    let metadata_bytes = fetch_json(client, version_url.as_str()).await?;
    let metadata: Value = serde_json::from_slice(&metadata_bytes)?;
    let metadata_id = metadata
        .get("id")
        .and_then(Value::as_str)
        .ok_or(VanillaInstallError::Missing("version id"))?;
    if metadata_id != version.id {
        return Err(VanillaInstallError::VersionMismatch {
            metadata: metadata_id.to_owned(),
            requested: version.id,
        });
    }

    let libraries = library_tasks(&metadata, &root)?;
    let last = Arc::new(AtomicUsize::new(0));
    let library_progress = throttled_progress(
        Arc::clone(&progress),
        Arc::clone(&last),
        |completed, total| InstallProgress::DownloadingLibraries { completed, total },
    );
    download_all_with_progress(client, libraries, DOWNLOAD_CONCURRENCY, library_progress).await?;

    progress(InstallProgress::DownloadingAssetIndex);
    let asset_index_info = metadata
        .get("assetIndex")
        .ok_or(VanillaInstallError::Missing("asset index"))?;
    let asset_index_id = asset_index_info
        .get("id")
        .and_then(Value::as_str)
        .ok_or(VanillaInstallError::Missing("asset index id"))?;
    if !valid_version_id(asset_index_id) {
        return Err(VanillaInstallError::Invalid("asset index id"));
    }
    let asset_index_url = asset_index_info
        .get("url")
        .and_then(Value::as_str)
        .ok_or(VanillaInstallError::Missing("asset index URL"))?;
    let asset_index_path = safe_destination(
        &root.join("assets").join("indexes"),
        &format!("{asset_index_id}.json"),
    )?;
    let mut asset_index_task = DownloadTask::new(
        asset_index_url,
        asset_index_path.clone(),
        asset_index_info
            .get("sha1")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if let Some(size) = asset_index_info.get("size").and_then(Value::as_u64) {
        asset_index_task = asset_index_task.with_size(size);
    }
    download_all_with_progress(client, vec![asset_index_task], 1, |_, _| {}).await?;
    let asset_index_bytes = tokio::fs::read(&asset_index_path).await?;
    if asset_index_bytes.len() > MAX_METADATA_BYTES {
        return Err(VanillaInstallError::Invalid("asset index size"));
    }
    let asset_index: AssetIndex = serde_json::from_slice(&asset_index_bytes)?;

    let assets = asset_tasks(&asset_index, &root)?;
    let last = Arc::new(AtomicUsize::new(0));
    let asset_progress = throttled_progress(Arc::clone(&progress), last, |completed, total| {
        InstallProgress::DownloadingAssets { completed, total }
    });
    download_all_with_progress(client, assets, DOWNLOAD_CONCURRENCY, asset_progress).await?;

    progress(InstallProgress::DownloadingClient);
    let client_info = metadata
        .pointer("/downloads/client")
        .ok_or(VanillaInstallError::Missing("client download"))?;
    let client_url = client_info
        .get("url")
        .and_then(Value::as_str)
        .ok_or(VanillaInstallError::Missing("client URL"))?;
    let client_path = safe_destination(
        &root.join("versions").join(&version.id),
        &format!("{}.jar", version.id),
    )?;
    let mut client_task = DownloadTask::new(
        client_url,
        client_path,
        client_info
            .get("sha1")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if let Some(size) = client_info.get("size").and_then(Value::as_u64) {
        client_task = client_task.with_size(size);
    }
    download_all_with_progress(client, vec![client_task], 1, |_, _| {}).await?;

    let root_for_assets = root.clone();
    progress(InstallProgress::MaterializingAssets);
    tokio::task::spawn_blocking(move || materialize_legacy_assets(&root_for_assets, &asset_index))
        .await
        .map_err(|error| VanillaInstallError::Join(error.to_string()))??;

    progress(InstallProgress::Finalizing);
    let metadata_path = safe_destination(
        &root.join("versions").join(&version.id),
        &format!("{}.json", version.id),
    )?;
    let metadata_bytes = serde_json::to_vec(&metadata)?;
    write_version_metadata(&metadata_path, &metadata_bytes)?;
    // A completed install is recorded so the next launch only re-checks the
    // cheap paths. A failure here costs one deep scan, never a repair.
    let _ = write_complete_marker(&root, &version.id);
    progress(InstallProgress::Complete);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_safe_maven_coordinates() {
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3:natives-windows@zip").unwrap(),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.zip"
        );
        assert_eq!(
            maven_path("com.google.guava:guava:33.0.0-jre").unwrap(),
            "com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar"
        );
    }

    #[test]
    fn rejects_unsafe_paths_and_maven_coordinates() {
        for path in ["", "../outside.jar", "a/../../b", "/rooted", r"a\b", "C:/x"] {
            assert!(safe_relative_path(path).is_err(), "accepted {path:?}");
        }
        for coordinate in ["../../x:y:z", "group::version", "group:name:version:../x"] {
            assert!(maven_path(coordinate).is_err(), "accepted {coordinate:?}");
        }
    }

    #[test]
    fn validates_version_ids_for_path_use() {
        assert!(valid_version_id("1.21.1"));
        assert!(valid_version_id("26.3-snapshot-1"));
        assert!(!valid_version_id("../outside"));
        assert!(!valid_version_id("with space"));
    }

    #[test]
    fn platform_filter_selects_only_the_matching_maven_native_classifier() {
        let windows_x64 = serde_json::json!({
            "name": "org.lwjgl:lwjgl:3.4.3:natives-windows",
            "rules": [{"action":"allow","os":{"name":"windows"}}]
        });
        let windows_arm64 = serde_json::json!({
            "name": "org.lwjgl:lwjgl:3.4.3:natives-windows-arm64",
            "rules": [{"action":"allow","os":{"name":"windows"}}]
        });
        assert_eq!(
            library_allowed(&windows_x64),
            cfg!(all(target_os = "windows", target_arch = "x86_64"))
        );
        assert!(!library_allowed(&windows_arm64));
    }

    fn health_fixture(root: &Path) {
        let version_dir = root.join("versions").join("1.21.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join("1.21.1.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "1.21.1",
                "libraries": [{
                    "name": "org.ow2.asm:asm:9.7",
                    "downloads": {
                        "artifact": {
                            "path": "org/ow2/asm/asm/9.7/asm-9.7.jar",
                            "url": "https://libraries.minecraft.net/org/ow2/asm/asm/9.7/asm-9.7.jar",
                            "sha1": "0000000000000000000000000000000000000000",
                            "size": 1
                        }
                    }
                }],
                "assetIndex": {"id": "5", "url": "https://piston-meta.mojang.com/x.json"}
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(version_dir.join("1.21.1.jar"), b"jar").unwrap();
    }

    #[test]
    fn missing_install_files_names_the_gap_and_clears_when_filled() {
        let root = std::env::temp_dir().join(format!(
            "kaelis-health-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        // Nothing installed: the version metadata is the first gap.
        assert_eq!(
            missing_install_files(&root, "1.21.1").unwrap().len(),
            1,
            "an uninstalled version reports its metadata"
        );
        health_fixture(&root);
        // Metadata and client jar exist, so the library and asset index show up.
        let missing = missing_install_files(&root, "1.21.1").unwrap();
        assert_eq!(
            missing.len(),
            2,
            "library jar plus asset index: {missing:?}"
        );
        assert!(missing.iter().any(|path| path.ends_with("asm-9.7.jar")));
        assert!(missing.iter().any(|path| path.ends_with("indexes/5.json")));
        // Fill both, plus an empty object index: a clean install reports none.
        let library = root.join("libraries/org/ow2/asm/asm/9.7/asm-9.7.jar");
        std::fs::create_dir_all(library.parent().unwrap()).unwrap();
        std::fs::write(&library, b"jar").unwrap();
        let index = root.join("assets/indexes/5.json");
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        std::fs::write(&index, br#"{"objects":{}}"#).unwrap();
        assert!(missing_install_files(&root, "1.21.1").unwrap().is_empty());
        // The completion marker is what makes the asset pass one-time work.
        assert!(!install_complete(&root, "1.21.1"));
        write_complete_marker(&root, "1.21.1").unwrap();
        assert!(install_complete(&root, "1.21.1"));
        assert!(missing_install_files(&root, "1.21.1").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_marker_for_another_version_is_not_proof() {
        let root = std::env::temp_dir().join(format!(
            "kaelis-marker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("versions/1.21.1")).unwrap();
        write_complete_marker(&root, "1.20.4").unwrap();
        assert!(!install_complete(&root, "1.21.1"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
