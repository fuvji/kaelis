//! Install Mojang's version-matched Java runtime into an instance-local folder.

use super::downloader::{DownloadError, DownloadTask, download_all_with_progress};
use super::vanilla;
use kaelis_core::instance::Instance;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use thiserror::Error;

pub const RUNTIME_CATALOG_URL: &str = "https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;
const RUNTIME_CONCURRENCY: usize = 12;

#[derive(Debug, Error)]
pub enum RuntimeInstallError {
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
    #[error("unsafe runtime path: {0}")]
    UnsafePath(String),
    #[error("this Mojang runtime manifest contains an unsupported link: {0}")]
    UnsupportedLink(String),
    #[error("runtime manifest hash mismatch: expected {expected}, got {actual}")]
    ManifestHashMismatch { expected: String, actual: String },
    #[error("blocking runtime preparation task failed: {0}")]
    Join(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeInstallProgress {
    Resolving,
    Downloading { completed: usize, total: usize },
    Complete,
}

impl RuntimeInstallProgress {
    pub fn message(self) -> String {
        match self {
            Self::Resolving => "Resolving the compatible Mojang Java runtime…".to_owned(),
            Self::Downloading { completed, total } => {
                format!("Downloading Java runtime ({completed}/{total})…")
            }
            Self::Complete => "Java runtime installed.".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    files: std::collections::HashMap<String, RuntimeFile>,
}

#[derive(Debug, Deserialize)]
struct RuntimeFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    downloads: RuntimeDownloads,
    #[serde(default)]
    executable: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RuntimeDownloads {
    raw: Option<RuntimeDownload>,
}

#[derive(Debug, Deserialize)]
struct RuntimeDownload {
    url: String,
    sha1: String,
    size: u64,
}

/// Map a host OS/arch pair to a Mojang runtime catalogue key.
///
/// Split from [`runtime_platform`] so the whole table can be tested on every
/// build, instead of a single hard-coded host assertion.
fn platform_key(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("windows-x64"),
        ("windows", "x86") => Some("windows-x86"),
        ("windows", "aarch64") => Some("windows-arm64"),
        ("linux", "x86_64") => Some("linux"),
        ("linux", "x86") => Some("linux-i386"),
        ("macos", "x86_64") => Some("mac-os"),
        ("macos", "aarch64") => Some("mac-os-arm64"),
        _ => None,
    }
}

fn runtime_platform() -> Result<&'static str, RuntimeInstallError> {
    // Mojang publishes no managed runtime for Linux on ARM; those users run a
    // system JDK, which `resolve_java` finds on PATH or in a JDK folder.
    platform_key(std::env::consts::OS, std::env::consts::ARCH).ok_or(RuntimeInstallError::Invalid(
        "a Mojang-managed Java runtime",
    ))
}

fn safe_runtime_path(root: &Path, relative: &str) -> Result<PathBuf, RuntimeInstallError> {
    vanilla::safe_destination(root, relative).map_err(|error| match error {
        vanilla::VanillaInstallError::UnsafePath(path) => RuntimeInstallError::UnsafePath(path),
        vanilla::VanillaInstallError::Io(error) => RuntimeInstallError::Io(error),
        _ => RuntimeInstallError::Invalid("runtime destination"),
    })
}

async fn fetch_json(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, RuntimeInstallError> {
    let url = reqwest::Url::parse(url).map_err(|_| RuntimeInstallError::Invalid("URL"))?;
    if url.scheme() != "https" {
        return Err(RuntimeInstallError::Invalid("HTTPS metadata URL"));
    }
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(url.clone())
            .header("User-Agent", kaelis_core::brand::user_agent())
    })
    .await?
    .error_for_status()?;
    if response.url().scheme() != "https"
        || response
            .content_length()
            .is_some_and(|length| length > MAX_METADATA_BYTES as u64)
    {
        return Err(RuntimeInstallError::Invalid(
            "metadata response URL or size",
        ));
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(RuntimeInstallError::Invalid("metadata response size"));
    }
    Ok(bytes.to_vec())
}

fn runtime_executable(root: &Path) -> PathBuf {
    root.join("bin").join(java_binary_name())
}

/// Platform file name of the Java launcher.
pub fn java_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    }
}

/// Sort key for runtime directory names such as `21.0.5` or `25`.
fn version_rank(name: &str) -> Vec<u32> {
    name.split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect()
}

/// The Java executable of the newest Mojang runtime installed inside one
/// instance (`runtime/<component>/<version>/bin/java`), if any.
///
/// Launching used to depend on a bare `java.exe` from `PATH`, so an instance
/// with a perfectly good managed runtime still failed with "program not
/// found" on machines without a system JDK.
pub fn managed_java(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for component in std::fs::read_dir(root.join("runtime")).ok()?.flatten() {
        if !component.path().is_dir() {
            continue;
        }
        let Ok(versions) = std::fs::read_dir(component.path()) else {
            continue;
        };
        for version in versions.flatten() {
            let java = runtime_executable(&version.path());
            if !is_executable(&java) {
                continue;
            }
            let rank = version_rank(&version.file_name().to_string_lossy());
            if best.as_ref().is_none_or(|(current, _)| rank > *current) {
                best = Some((rank, java));
            }
        }
    }
    best.map(|(_, java)| java)
}

/// Accept either a `java` executable or a JDK home directory in the editor.
pub fn java_from_setting(path: &Path) -> Option<PathBuf> {
    if is_executable(path) {
        return Some(path.to_path_buf());
    }
    let nested = runtime_executable(path);
    is_executable(&nested).then_some(nested)
}

/// First `java` on `PATH`, used only when an instance has no managed runtime.
pub fn java_on_path() -> Option<PathBuf> {
    let value = std::env::var_os("PATH")?;
    std::env::split_paths(&value)
        .map(|directory| directory.join(java_binary_name()))
        .find(|candidate| is_executable(candidate))
}

/// A runnable file, including the Unix execute bit.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    true
}

/// JDK install roots per platform. A desktop shortcut, Dock launch, or menu
/// entry does not inherit the shell `PATH`, so `java` on `PATH` alone is not
/// enough on macOS and Linux.
fn java_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        for base in [
            "C:\\Program Files\\Java",
            "C:\\Program Files\\Eclipse Adoptium",
            "C:\\Program Files\\Microsoft",
            "C:\\Program Files\\Zulu",
            "C:\\Program Files\\BellSoft",
        ] {
            roots.push(PathBuf::from(base));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Eclipse Adoptium"),
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        roots.push(PathBuf::from("/System/Library/Java/JavaVirtualMachines"));
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join("Library/Java/JavaVirtualMachines"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        roots.push(PathBuf::from("/usr/lib/jvm"));
        roots.push(PathBuf::from("/usr/java"));
        roots.push(PathBuf::from("/opt/java"));
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join(".jdks"));
        }
    }
    roots
}

/// Best system JDK when the launcher has not provisioned one.
///
/// Newest major version wins, so a machine with a leftover Java 8 next to a
/// current JDK still launches modern Minecraft.
pub fn system_java() -> Option<PathBuf> {
    let mut best: Option<((u32, String), PathBuf)> = None;
    for root in java_search_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            if !directory.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let major = java_major_hint(&name);
            for candidate in [
                // Plain JDK layout.
                directory.join("bin").join(java_binary_name()),
                // macOS bundles keep the JDK under Contents/Home.
                directory
                    .join("Contents")
                    .join("Home")
                    .join("bin")
                    .join(java_binary_name()),
            ] {
                if !is_executable(&candidate) {
                    continue;
                }
                let rank = (major, name.clone());
                if best.as_ref().is_none_or(|(current, _)| rank > *current) {
                    best = Some((rank, candidate));
                }
            }
        }
    }
    best.map(|(_, java)| java)
}

/// Major version implied by an install directory name such as `jdk-21.0.5`,
/// `temurin-17.jdk`, or `java-1.8.0-openjdk`. Returns 0 when no number exists.
fn java_major_hint(name: &str) -> u32 {
    let mut digits = String::new();
    let mut best = 0;
    for character in name.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        if !digits.is_empty() {
            best = best.max(digits.parse().unwrap_or(0));
            digits.clear();
        }
    }
    best.max(digits.parse().unwrap_or(0))
}

/// The Java this instance would actually start: the configured path, else its
/// managed runtime, else `PATH`, else an installed system JDK.
pub fn resolve_java(instance: &Instance) -> Option<PathBuf> {
    instance
        .java_path
        .as_deref()
        .and_then(java_from_setting)
        .or_else(|| managed_java(&instance.root))
        .or_else(java_on_path)
        .or_else(system_java)
}

fn validate_runtime_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn stage_progress<F>(
    progress: Arc<F>,
    last_reported: Arc<AtomicUsize>,
) -> impl Fn(usize, usize) + Send + Sync + 'static
where
    F: Fn(RuntimeInstallProgress) + Send + Sync + 'static,
{
    move |completed, total| {
        let step = (total / 100).max(1);
        let previous = last_reported.load(Ordering::Relaxed);
        if completed == 0 || completed == total || completed.saturating_sub(previous) >= step {
            last_reported.store(completed, Ordering::Relaxed);
            progress(RuntimeInstallProgress::Downloading { completed, total });
        }
    }
}

/// Install the Mojang runtime component and major version required by the
/// instance's installed vanilla version metadata.
pub async fn install_for_instance<F>(
    client: &reqwest::Client,
    instance: &Instance,
    progress: F,
) -> Result<PathBuf, RuntimeInstallError>
where
    F: Fn(RuntimeInstallProgress) + Send + Sync + 'static,
{
    let progress = Arc::new(progress);
    progress(RuntimeInstallProgress::Resolving);
    if !vanilla::valid_version_id(&instance.mc_version) {
        return Err(RuntimeInstallError::Invalid(
            "instance Minecraft version id",
        ));
    }
    let version_path = instance
        .root
        .join("versions")
        .join(&instance.mc_version)
        .join(format!("{}.json", instance.mc_version));
    let version_bytes = tokio::fs::read(version_path).await?;
    if version_bytes.len() > MAX_METADATA_BYTES {
        return Err(RuntimeInstallError::Invalid("version metadata size"));
    }
    let version_metadata: Value = serde_json::from_slice(&version_bytes)?;
    if version_metadata.get("id").and_then(Value::as_str) != Some(instance.mc_version.as_str()) {
        return Err(RuntimeInstallError::Invalid(
            "instance version metadata identity",
        ));
    }
    let component = version_metadata
        .pointer("/javaVersion/component")
        .and_then(Value::as_str)
        .ok_or(RuntimeInstallError::Missing("javaVersion.component"))?;
    if !validate_runtime_name(component) {
        return Err(RuntimeInstallError::Invalid("Java runtime component"));
    }
    let major = version_metadata
        .pointer("/javaVersion/majorVersion")
        .and_then(Value::as_u64)
        .ok_or(RuntimeInstallError::Missing("javaVersion.majorVersion"))?;
    let major_prefix = format!("{major}.");

    let catalog_bytes = fetch_json(client, RUNTIME_CATALOG_URL).await?;
    let catalog: Value = serde_json::from_slice(&catalog_bytes)?;
    let platform = runtime_platform()?;
    let candidates = catalog
        .get(platform)
        .and_then(|value| value.get(component))
        .and_then(Value::as_array)
        .ok_or(RuntimeInstallError::Missing(
            "compatible runtime catalog entry",
        ))?;
    let candidate = candidates
        .iter()
        .filter(|entry| {
            entry
                .pointer("/version/name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == major.to_string() || name.starts_with(&major_prefix))
        })
        .max_by_key(|entry| {
            entry
                .pointer("/version/released")
                .and_then(Value::as_str)
                .unwrap_or_default()
        })
        .ok_or(RuntimeInstallError::Missing(
            "matching Java runtime version",
        ))?;
    let runtime_name = candidate
        .pointer("/version/name")
        .and_then(Value::as_str)
        .ok_or(RuntimeInstallError::Missing("runtime version name"))?;
    if !validate_runtime_name(runtime_name) {
        return Err(RuntimeInstallError::Invalid("runtime version name"));
    }
    let manifest_url = candidate
        .pointer("/manifest/url")
        .and_then(Value::as_str)
        .ok_or(RuntimeInstallError::Missing("runtime file manifest URL"))?;
    let manifest_info = candidate
        .get("manifest")
        .ok_or(RuntimeInstallError::Missing("runtime manifest metadata"))?;
    let runtime_bytes = fetch_json(client, manifest_url).await?;
    if manifest_info
        .get("size")
        .and_then(Value::as_u64)
        .is_some_and(|expected| runtime_bytes.len() as u64 != expected)
    {
        return Err(RuntimeInstallError::Invalid("runtime manifest size"));
    }
    if let Some(expected_sha1) = manifest_info.get("sha1").and_then(Value::as_str) {
        let actual_sha1 = super::hash::sha1_bytes(&runtime_bytes);
        if !actual_sha1.eq_ignore_ascii_case(expected_sha1) {
            return Err(RuntimeInstallError::ManifestHashMismatch {
                expected: expected_sha1.to_owned(),
                actual: actual_sha1,
            });
        }
    }
    let runtime_manifest: RuntimeManifest = serde_json::from_slice(&runtime_bytes)?;
    let runtime_root = instance
        .root
        .join("runtime")
        .join(component)
        .join(runtime_name);
    let mut tasks = Vec::with_capacity(runtime_manifest.files.len());
    let mut destinations = HashSet::with_capacity(runtime_manifest.files.len());
    let mut executable_files = Vec::new();
    for (relative, file) in &runtime_manifest.files {
        match file.kind.as_str() {
            "directory" => continue,
            "link" => return Err(RuntimeInstallError::UnsupportedLink(relative.clone())),
            "file" => {}
            _ => return Err(RuntimeInstallError::Invalid("runtime file type")),
        }
        let download = file
            .downloads
            .raw
            .as_ref()
            .ok_or(RuntimeInstallError::Missing("runtime raw file download"))?;
        let destination = safe_runtime_path(&runtime_root, relative)?;
        let target_key = destination.to_string_lossy().to_lowercase();
        if !destinations.insert(target_key) {
            return Err(RuntimeInstallError::Invalid(
                "duplicate runtime destination",
            ));
        }
        tasks.push(
            DownloadTask::new(&download.url, destination.clone(), &download.sha1)
                .with_size(download.size),
        );
        if file.executable {
            executable_files.push(destination);
        }
    }
    let last_reported = Arc::new(AtomicUsize::new(0));
    let callback = stage_progress(Arc::clone(&progress), last_reported);
    download_all_with_progress(client, tasks, RUNTIME_CONCURRENCY, callback).await?;

    #[cfg(unix)]
    for path in executable_files {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(&path).await?.permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(path, permissions).await?;
    }
    #[cfg(not(unix))]
    drop(executable_files);

    let java = runtime_executable(&runtime_root);
    if !java.is_file() {
        return Err(RuntimeInstallError::Missing("installed java executable"));
    }
    progress(RuntimeInstallProgress::Complete);
    Ok(java)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_host_targets_to_mojang_catalog_platforms() {
        for (os, arch, expected) in [
            ("windows", "x86_64", "windows-x64"),
            ("windows", "x86", "windows-x86"),
            ("windows", "aarch64", "windows-arm64"),
            ("linux", "x86_64", "linux"),
            ("linux", "x86", "linux-i386"),
            ("macos", "x86_64", "mac-os"),
            ("macos", "aarch64", "mac-os-arm64"),
        ] {
            assert_eq!(platform_key(os, arch), Some(expected), "{os}/{arch}");
        }
        assert_eq!(platform_key("freebsd", "x86_64"), None);
        assert_eq!(platform_key("windows", "mips"), None);
        // Mojang publishes no managed runtime for Linux on ARM.
        assert_eq!(platform_key("linux", "aarch64"), None);
        // The host itself must resolve on every supported target.
        assert!(runtime_platform().is_ok());
    }

    #[test]
    fn finds_the_newest_complete_managed_runtime() {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "kaelis-managed-java-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (component, version, complete) in [
            ("java-runtime-gamma", "17.0.8", true),
            ("java-runtime-delta", "21.0.5", true),
            ("java-runtime-epsilon", "25.0", false),
        ] {
            let bin = root
                .join("runtime")
                .join(component)
                .join(version)
                .join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            if complete {
                std::fs::write(bin.join(java_binary_name()), b"stub").unwrap();
            }
        }

        let found = managed_java(&root).expect("managed runtime");

        assert!(
            found.to_string_lossy().contains("java-runtime-delta")
                && found.to_string_lossy().contains("21.0.5"),
            "unexpected runtime: {}",
            found.display()
        );
        assert!(managed_java(&root.join("missing")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn accepts_a_jdk_home_in_the_java_setting() {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "kaelis-jdk-home-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(home.join("bin")).unwrap();
        let java = home.join("bin").join(java_binary_name());
        std::fs::write(&java, b"stub").unwrap();

        assert_eq!(java_from_setting(&home), Some(java.clone()));
        assert_eq!(java_from_setting(&java), Some(java.clone()));
        assert_eq!(java_from_setting(&home.join("nope")), None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn reads_the_java_major_from_install_folder_names() {
        assert_eq!(java_major_hint("jdk-21.0.5"), 21);
        assert_eq!(java_major_hint("temurin-17.jdk"), 17);
        assert_eq!(java_major_hint("java-1.8.0-openjdk"), 8);
        assert_eq!(java_major_hint("zulu25"), 25);
        assert_eq!(java_major_hint("latest"), 0);
    }

    #[test]
    fn validates_runtime_directory_components() {
        assert!(validate_runtime_name("java-runtime-epsilon"));
        assert!(validate_runtime_name("25.0.1"));
        assert!(!validate_runtime_name("../outside"));
        assert!(!validate_runtime_name("name with spaces"));
    }
}
