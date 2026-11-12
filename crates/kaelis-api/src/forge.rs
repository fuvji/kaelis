//! Forge and NeoForge provisioning.
//!
//! Fabric and Quilt publish a plain meta profile, so [`crate::loader`] can copy
//! their libraries itself. Forge and NeoForge instead ship a Java installer
//! that downloads their libraries and runs their own binary patches inside it.
//! Reimplementing that pipeline would duplicate Forge's tooling, so Kaelis
//! downloads the official installer from the loader's own Maven, verifies its
//! published SHA-1, runs it headlessly into the instance root, and then reads
//! the launcher profile the installer wrote. Nothing is guessed: an instance
//! counts as provisioned only when the profile names this Minecraft version and
//! every library and the patched client jar it needs are on disk.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use thiserror::Error;

/// Forge's Maven, which also serves every installer and library.
pub const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";
/// Forge's recommended/latest build table, keyed by `<minecraft>-<channel>`.
pub const FORGE_PROMOTIONS: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
/// NeoForge's Maven, which also serves every installer and library.
pub const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases";
const FORGE_METADATA: &str =
    "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";
const NEOFORGE_METADATA: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";
/// NeoForge 1.20.1 predates the `neoforge` artifact and lives under `forge`.
const NEOFORGE_LEGACY_METADATA: &str =
    "https://maven.neoforged.net/releases/net/neoforged/forge/maven-metadata.xml";
const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// How much installer output an error keeps.
const OUTPUT_TAIL: usize = 900;
static INSTALLER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum ForgeError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("download failed: {0}")]
    Download(#[from] crate::downloader::DownloadError),
    #[error("{0} is not a loader Kaelis can provision")]
    Unsupported(String),
    #[error("{loader} publishes no build for Minecraft {version}")]
    NoBuild {
        loader: &'static str,
        version: String,
    },
    #[error(
        "{loader} needs the vanilla client first; install Minecraft {version} for this instance, then retry"
    )]
    NoVanilla {
        loader: &'static str,
        version: String,
    },
    #[error("{loader} installer has no valid SHA-1 at {url}")]
    MissingChecksum { loader: &'static str, url: String },
    #[error("{loader} installer timed out after {seconds} seconds")]
    InstallerTimeout { loader: &'static str, seconds: u64 },
    #[error("{loader} installer exited with {status}: {tail}")]
    InstallerFailed {
        loader: &'static str,
        status: String,
        tail: String,
    },
    #[error("{loader} installer finished but wrote no launchable profile: {tail}")]
    NoProfile { loader: &'static str, tail: String },
    #[error("untrusted metadata URL: {0}")]
    InvalidUrl(String),
}

/// A Forge-family launch profile found under an instance's `versions/` tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeInstall {
    /// `forge` or `neoforge`.
    pub loader: String,
    pub game_version: String,
    /// Forge `47.4.0`, NeoForge `21.1.266`.
    pub loader_version: String,
    /// The launcher profile id (`1.20.1-forge-47.4.0`, `neoforge-21.1.266`).
    pub profile_id: String,
    pub main_class: String,
    /// The profile's own JVM arguments, `${library_directory}` still unexpanded.
    pub jvm_arguments: Vec<String>,
    /// The profile's game arguments, such as `--launchTarget forgeclient`.
    pub game_arguments: Vec<String>,
    pub libraries: usize,
    /// Relative Maven paths, so a partially deleted install is detected.
    pub library_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Forge,
    NeoForge,
}

impl Family {
    fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "forge" => Some(Self::Forge),
            "neoforge" => Some(Self::NeoForge),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
        }
    }

    fn metadata(self) -> &'static str {
        match self {
            Self::Forge => FORGE_METADATA,
            Self::NeoForge => NEOFORGE_METADATA,
        }
    }
}

/// Read an installed Forge or NeoForge profile without touching the network.
///
/// `None` means "not launchable yet": no profile for this exact Minecraft
/// version, a profile naming another version, a missing library, or a missing
/// patched client jar. A half-written install therefore gets repaired instead
/// of being launched into a stack trace.
pub fn load_installed(root: &Path, game_version: &str, loader: &str) -> Option<ForgeInstall> {
    let family = Family::from_name(loader)?;
    let libraries_root = root.join("libraries");
    let mut best: Option<(SystemTime, ForgeInstall)> = None;
    for entry in std::fs::read_dir(root.join("versions")).ok()?.flatten() {
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let profile_path = directory.join(format!("{name}.json"));
        let Ok(bytes) = std::fs::read(&profile_path) else {
            continue;
        };
        let Ok(profile) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let id = profile
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(name.as_str());
        if !id_matches(family, id, game_version) {
            continue;
        }
        if profile.get("inheritsFrom").and_then(Value::as_str) != Some(game_version) {
            continue;
        }
        let Some(main_class) = profile
            .get("mainClass")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some(loader_version) = loader_version_for(id, game_version) else {
            continue;
        };
        // The patched client jar is the authority on which family produced the
        // profile: NeoForge 1.20.1 reuses the `1.20.1-forge-<build>` id but
        // writes its jar under `net/neoforged/forge`.
        if !patched_client_exists(family, &libraries_root, game_version, &loader_version) {
            continue;
        }
        let Some(library_paths) = verified_library_paths(&libraries_root, &profile) else {
            continue;
        };
        let modified = std::fs::metadata(&profile_path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let install = ForgeInstall {
            loader: family.name().to_owned(),
            game_version: game_version.to_owned(),
            loader_version,
            profile_id: id.to_owned(),
            main_class: main_class.to_owned(),
            jvm_arguments: argument_list(&profile, "jvm"),
            game_arguments: argument_list(&profile, "game"),
            libraries: library_paths.len(),
            library_paths,
        };
        if best.as_ref().is_none_or(|(current, _)| modified > *current) {
            best = Some((modified, install));
        }
    }
    best.map(|(_, install)| install)
}

/// Install Forge or NeoForge with its official headless installer.
///
/// The vanilla client must already be present, because the installer patches
/// it. `java` is the runtime that will also launch the game; Forge's installer
/// and the game it installs require the same major version.
pub async fn install(
    client: &reqwest::Client,
    root: &Path,
    game_version: &str,
    loader: &str,
    java: &Path,
) -> Result<ForgeInstall, ForgeError> {
    let family =
        Family::from_name(loader).ok_or_else(|| ForgeError::Unsupported(loader.to_owned()))?;
    if !vanilla_ready(root, game_version) {
        return Err(ForgeError::NoVanilla {
            loader: family.label(),
            version: game_version.to_owned(),
        });
    }
    let url = resolve_installer(client, family, game_version).await?;
    let checksum_url = format!("{url}.sha1");
    let checksum_text = fetch_text(client, &checksum_url).await?;
    let checksum = valid_sha1(&checksum_text).ok_or_else(|| ForgeError::MissingChecksum {
        loader: family.label(),
        url: checksum_url,
    })?;
    let installer = InstallerFile::new(family)?;
    crate::downloader::download_all(
        client,
        vec![crate::downloader::DownloadTask::new(
            &url,
            installer.path.clone(),
            &checksum,
        )],
        1,
    )
    .await?;
    ensure_launcher_profiles(root)?;
    let output = run_installer(family, java, &installer.path, root).await?;
    load_installed(root, game_version, family.name()).ok_or_else(|| ForgeError::NoProfile {
        loader: family.label(),
        tail: tail(&output),
    })
}

fn vanilla_ready(root: &Path, game_version: &str) -> bool {
    let directory = root.join("versions").join(game_version);
    directory.join(format!("{game_version}.json")).is_file()
        && directory.join(format!("{game_version}.jar")).is_file()
}

async fn resolve_installer(
    client: &reqwest::Client,
    family: Family,
    game_version: &str,
) -> Result<String, ForgeError> {
    if family == Family::Forge {
        // Promotions is the curated table the download page uses. A stale or
        // unreachable table is not fatal: Maven's full list is the fallback.
        if let Ok(text) = fetch_text(client, FORGE_PROMOTIONS).await
            && let Some(version) = forge_promo_version(&text, game_version)
        {
            return Ok(forge_installer_url(game_version, &version));
        }
        let metadata = fetch_text(client, family.metadata()).await?;
        let version =
            pick_prefixed_build(&metadata, game_version).ok_or_else(|| ForgeError::NoBuild {
                loader: family.label(),
                version: game_version.to_owned(),
            })?;
        return Ok(forge_installer_url(game_version, &version));
    }
    // NeoForge changed coordinates after 1.20.1: that one release lives under
    // `net.neoforged:forge` with `<minecraft>-<build>` versions, every later
    // one under `net.neoforged:neoforge` with bare build versions.
    if game_version == "1.20.1" {
        let metadata = fetch_text(client, NEOFORGE_LEGACY_METADATA).await?;
        let version =
            pick_prefixed_build(&metadata, game_version).ok_or_else(|| ForgeError::NoBuild {
                loader: family.label(),
                version: game_version.to_owned(),
            })?;
        let full = format!("{game_version}-{version}");
        return Ok(format!(
            "{base}/net/neoforged/forge/{full}/forge-{full}-installer.jar",
            base = NEOFORGE_MAVEN
        ));
    }
    let metadata = fetch_text(client, family.metadata()).await?;
    let version =
        pick_neoforge_build(&metadata, game_version).ok_or_else(|| ForgeError::NoBuild {
            loader: family.label(),
            version: game_version.to_owned(),
        })?;
    Ok(format!(
        "{base}/net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar",
        base = NEOFORGE_MAVEN
    ))
}

fn forge_installer_url(game_version: &str, version: &str) -> String {
    let full = format!("{game_version}-{version}");
    format!(
        "{base}/net/minecraftforge/forge/{full}/forge-{full}-installer.jar",
        base = FORGE_MAVEN
    )
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, ForgeError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| ForgeError::InvalidUrl(url.to_owned()))?;
    if parsed.scheme() != "https" {
        return Err(ForgeError::InvalidUrl(url.to_owned()));
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
        return Err(ForgeError::InvalidUrl(response.url().to_string()));
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(ForgeError::InvalidUrl(url.to_owned()));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The Forge installer refuses a directory without a launcher profile and
/// injects its own entry into one. An instance is self-contained, so Kaelis
/// creates the minimal file the installer expects instead of asking the user
/// to run another launcher first.
fn ensure_launcher_profiles(root: &Path) -> Result<(), ForgeError> {
    let standard = root.join("launcher_profiles.json");
    let microsoft_store = root.join("launcher_profiles_microsoft_store.json");
    if standard.is_file() || microsoft_store.is_file() {
        return Ok(());
    }
    std::fs::write(&standard, b"{\n  \"profiles\": {}\n}\n")?;
    Ok(())
}

async fn run_installer(
    family: Family,
    java: &Path,
    installer: &Path,
    root: &Path,
) -> Result<String, ForgeError> {
    let mut command = tokio::process::Command::new(java);
    command
        .arg("-jar")
        .arg(installer)
        .arg("--installClient")
        .arg(root)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x0800_0000);
    }
    let output = tokio::time::timeout(INSTALL_TIMEOUT, command.output())
        .await
        .map_err(|_| ForgeError::InstallerTimeout {
            loader: family.label(),
            seconds: INSTALL_TIMEOUT.as_secs(),
        })?
        .map_err(ForgeError::Io)?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        return Err(ForgeError::InstallerFailed {
            loader: family.label(),
            status: output.status.to_string(),
            tail: tail(&text),
        });
    }
    Ok(text)
}

/// A downloaded installer that removes itself however the install ends.
struct InstallerFile {
    path: PathBuf,
}

impl InstallerFile {
    fn new(family: Family) -> Result<Self, ForgeError> {
        let directory = kaelis_core::sweeper::temp_root().join("kaelis-installers");
        std::fs::create_dir_all(&directory)?;
        let sequence = INSTALLER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "{}-installer-{}-{sequence}.jar",
            family.name(),
            std::process::id()
        ));
        Ok(Self { path })
    }
}

impl Drop for InstallerFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn id_matches(family: Family, id: &str, game_version: &str) -> bool {
    // Modern ids are `1.20.1-forge-47.4.0`; Forge 1.12.2 wrote
    // `1.12.2-forge1.12.2-14.23.5.2860`, with the Minecraft version repeated
    // and no separating dash.
    let prefixed = id
        .strip_prefix(game_version)
        .is_some_and(|rest| rest.starts_with("-forge"));
    match family {
        Family::Forge => prefixed,
        Family::NeoForge => prefixed || id.contains("neoforge-"),
    }
}

fn loader_version_for(id: &str, game_version: &str) -> Option<String> {
    if let Some(rest) = id
        .strip_prefix(game_version)
        .and_then(|rest| rest.strip_prefix("-forge"))
    {
        let rest = rest.strip_prefix('-').unwrap_or(rest);
        return Some(
            rest.strip_prefix(&format!("{game_version}-"))
                .unwrap_or(rest)
                .to_owned(),
        );
    }
    id.strip_prefix("neoforge-")
        .or_else(|| id.rsplit_once("neoforge-").map(|(_, version)| version))
        .map(str::to_owned)
}

fn patched_client_exists(
    family: Family,
    libraries: &Path,
    game_version: &str,
    loader_version: &str,
) -> bool {
    match family {
        Family::Forge => {
            let full = format!("{game_version}-{loader_version}");
            libraries
                .join("net/minecraftforge/forge")
                .join(&full)
                .join(format!("forge-{full}-client.jar"))
                .is_file()
        }
        Family::NeoForge => {
            libraries
                .join("net/neoforged/neoforge")
                .join(loader_version)
                .join(format!("neoforge-{loader_version}-client.jar"))
                .is_file()
                || {
                    let full = format!("{game_version}-{loader_version}");
                    libraries
                        .join("net/neoforged/forge")
                        .join(&full)
                        .join(format!("forge-{full}-client.jar"))
                        .is_file()
                }
        }
    }
}

fn verified_library_paths(libraries_root: &Path, profile: &Value) -> Option<Vec<String>> {
    let libraries = profile.get("libraries")?.as_array()?;
    if libraries.is_empty() {
        return None;
    }
    let mut paths = Vec::with_capacity(libraries.len());
    for library in libraries {
        if !rule_allows(library.get("rules"), true) {
            continue;
        }
        let Some(relative) = library_path(library) else {
            continue;
        };
        if !libraries_root.join(&relative).is_file() {
            return None;
        }
        paths.push(relative);
    }
    (!paths.is_empty()).then_some(paths)
}

fn library_path(library: &Value) -> Option<String> {
    if let Some(path) = library
        .pointer("/downloads/artifact/path")
        .and_then(Value::as_str)
    {
        return Some(path.to_owned());
    }
    let name = library.get("name")?.as_str()?;
    crate::loader::maven_path(name)
}

fn argument_list(profile: &Value, key: &str) -> Vec<String> {
    let Some(items) = profile
        .pointer(&format!("/arguments/{key}"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut arguments = Vec::new();
    for item in items {
        match item {
            Value::String(value) => arguments.push(value.clone()),
            Value::Object(entry) => {
                if !rule_allows(entry.get("rules"), false) {
                    continue;
                }
                match entry.get("value") {
                    Some(Value::String(value)) => arguments.push(value.clone()),
                    Some(Value::Array(values)) => arguments.extend(
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToOwned::to_owned),
                    ),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    arguments
}

fn rule_allows(rules: Option<&Value>, default: bool) -> bool {
    let Some(rules) = rules.and_then(Value::as_array) else {
        return default;
    };
    let mut allowed = false;
    for rule in rules {
        let os_matches = rule
            .pointer("/os/name")
            .and_then(Value::as_str)
            .is_none_or(|name| name == platform_name());
        let arch_matches = rule
            .pointer("/os/arch")
            .and_then(Value::as_str)
            .is_none_or(|arch| arch == std::env::consts::ARCH);
        if os_matches && arch_matches {
            allowed = rule
                .get("action")
                .and_then(Value::as_str)
                .is_some_and(|action| action == "allow");
        }
    }
    allowed
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn forge_promo_version(text: &str, game_version: &str) -> Option<String> {
    let promotions: Value = serde_json::from_str(text).ok()?;
    let promotions = promotions.get("promos")?.as_object()?;
    for channel in ["recommended", "latest"] {
        if let Some(version) = promotions
            .get(&format!("{game_version}-{channel}"))
            .and_then(Value::as_str)
        {
            let version = version.trim();
            if is_version_component(version) {
                return Some(version.to_owned());
            }
        }
    }
    None
}

fn pick_prefixed_build(metadata: &str, game_version: &str) -> Option<String> {
    xml_versions(metadata)
        .into_iter()
        .filter_map(|candidate| {
            let rest = candidate.strip_prefix(game_version)?;
            let version = rest.strip_prefix('-')?;
            // A build suffix (`47.5.0-beta`) is not a stable download, and an
            // extra dash would mean a different Minecraft version, not this one.
            if version.contains('-') || !is_version_component(version) {
                return None;
            }
            Some((version.to_owned(), version_key(version)))
        })
        .max_by(|left, right| left.1.cmp(&right.1))
        .map(|(version, _)| version)
}

fn pick_neoforge_build(metadata: &str, game_version: &str) -> Option<String> {
    let prefixes = neoforge_prefixes(game_version);
    if prefixes.is_empty() {
        return None;
    }
    xml_versions(metadata)
        .into_iter()
        .filter(|candidate| {
            !candidate.contains('-')
                && is_version_component(candidate)
                && prefixes.iter().any(|prefix| candidate.starts_with(prefix))
        })
        .max_by(|left, right| version_key(left).cmp(&version_key(right)))
}

/// NeoForge numbers its builds by the Minecraft line: 1.21.1 is `21.1.x`,
/// 1.21 is `21.0.x`. 1.20.1 predates that scheme and lives under the legacy
/// `net.neoforged:forge` coordinate instead (see [`resolve_installer`]).
fn neoforge_prefixes(game_version: &str) -> Vec<String> {
    let mut parts = game_version.split('.');
    if parts.next() != Some("1") {
        return Vec::new();
    }
    let Some(minor) = parts.next() else {
        return Vec::new();
    };
    match parts.next() {
        Some(patch) => vec![format!("{minor}.{patch}.")],
        None => vec![format!("{minor}.0.")],
    }
}

fn xml_versions(xml: &str) -> Vec<String> {
    let mut versions = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<version>") {
        let after = &rest[start + "<version>".len()..];
        let Some(end) = after.find("</version>") else {
            break;
        };
        versions.push(after[..end].trim().to_owned());
        rest = &after[end + "</version>".len()..];
    }
    versions
}

fn version_key(value: &str) -> Vec<u64> {
    value
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn is_version_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_sha1(text: &str) -> Option<String> {
    let value = text.split_whitespace().next()?.trim();
    (value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn tail(text: &str) -> String {
    let text = text.trim();
    if text.len() <= OUTPUT_TAIL {
        return text.to_owned();
    }
    let start = text.len() - OUTPUT_TAIL;
    let start = text
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= start)
        .unwrap_or(text.len());
    text[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        forge_promo_version, load_installed, neoforge_prefixes, pick_neoforge_build,
        pick_prefixed_build, valid_sha1,
    };
    use std::path::{Path, PathBuf};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "kaelis-forge-{}-{}-{tag}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_profile(root: &Path, id: &str, minecraft: &str) {
        let directory = root.join("versions").join(id);
        std::fs::create_dir_all(&directory).unwrap();
        let profile = serde_json::json!({
            "id": id,
            "inheritsFrom": minecraft,
            "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
            "arguments": {
                "jvm": ["-DlibraryDirectory=${library_directory}"],
                "game": ["--launchTarget", "forgeclient"]
            },
            "libraries": [
                {"name": "net.minecraftforge:fmlloader:1.20.1-47.4.0"},
                {"name": "cpw.mods:bootstraplauncher:1.1.2"}
            ]
        });
        std::fs::write(
            directory.join(format!("{id}.json")),
            serde_json::to_vec(&profile).unwrap(),
        )
        .unwrap();
    }

    fn seed_jar(root: &Path, relative: &str) {
        let path = root.join("libraries").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"jar").unwrap();
    }

    fn seed_profile_libraries(root: &Path) {
        seed_jar(
            root,
            "net/minecraftforge/fmlloader/1.20.1-47.4.0/fmlloader-1.20.1-47.4.0.jar",
        );
        seed_jar(
            root,
            "cpw/mods/bootstraplauncher/1.1.2/bootstraplauncher-1.1.2.jar",
        );
    }

    #[test]
    fn a_forge_profile_is_recognised_only_with_its_libraries_and_patched_client() {
        let temp = TempRoot::new("profile");
        let root = temp.path();
        write_profile(root, "1.20.1-forge-47.4.0", "1.20.1");
        seed_profile_libraries(root);
        seed_jar(
            root,
            "net/minecraftforge/forge/1.20.1-47.4.0/forge-1.20.1-47.4.0-client.jar",
        );

        let install = load_installed(root, "1.20.1", "forge").expect("a complete profile");
        assert_eq!(install.loader, "forge");
        assert_eq!(install.loader_version, "47.4.0");
        assert_eq!(install.profile_id, "1.20.1-forge-47.4.0");
        assert_eq!(install.libraries, 2);
        assert_eq!(install.jvm_arguments.len(), 1);
        assert_eq!(
            install.game_arguments,
            vec!["--launchTarget".to_owned(), "forgeclient".to_owned()]
        );

        assert!(load_installed(root, "1.20.1", "neoforge").is_none());
        assert!(load_installed(root, "1.20.2", "forge").is_none());

        // A partially deleted install must not be reported as launchable.
        std::fs::remove_file(root.join(
            "libraries/net/minecraftforge/forge/1.20.1-47.4.0/forge-1.20.1-47.4.0-client.jar",
        ))
        .unwrap();
        assert!(load_installed(root, "1.20.1", "forge").is_none());
    }

    #[test]
    fn a_foreign_profile_is_never_mistaken_for_forge() {
        let temp = TempRoot::new("foreign");
        let root = temp.path();
        write_profile(root, "1.20.11-forge-1.0.0", "1.20.1");
        seed_profile_libraries(root);
        assert!(load_installed(root, "1.20.1", "forge").is_none());
        assert!(load_installed(root, "1.20.1", "fabric").is_none());
    }

    #[test]
    fn neoforge_1201_uses_the_legacy_forge_coordinate() {
        let temp = TempRoot::new("neoforge-legacy");
        let root = temp.path();
        // NeoForge 1.20.1 reuses the Forge profile id; only the patched jar
        // location says which family installed it.
        write_profile(root, "1.20.1-forge-47.1.106", "1.20.1");
        seed_profile_libraries(root);
        seed_jar(
            root,
            "net/neoforged/forge/1.20.1-47.1.106/forge-1.20.1-47.1.106-client.jar",
        );

        let install = load_installed(root, "1.20.1", "neoforge").expect("legacy NeoForge");
        assert_eq!(install.loader, "neoforge");
        assert_eq!(install.loader_version, "47.1.106");
        assert!(
            load_installed(root, "1.20.1", "forge").is_none(),
            "a NeoForge install must not be launched as Forge"
        );
    }

    #[test]
    fn legacy_forge_ids_without_a_separating_dash_are_recognised() {
        let temp = TempRoot::new("legacy-forge-id");
        let root = temp.path();
        write_profile(root, "1.12.2-forge1.12.2-14.23.5.2860", "1.12.2");
        seed_profile_libraries(root);
        let full = "1.12.2-14.23.5.2860";
        seed_jar(
            root,
            &format!("net/minecraftforge/forge/{full}/forge-{full}-client.jar"),
        );

        let install = load_installed(root, "1.12.2", "forge").expect("legacy Forge id");
        assert_eq!(install.loader_version, "14.23.5.2860");
        assert_eq!(install.profile_id, "1.12.2-forge1.12.2-14.23.5.2860");
    }

    #[test]
    fn forge_promotions_prefer_recommended_then_latest() {
        let promotions = r#"{"promos":{"1.20.1-recommended":"47.4.0","1.20.1-latest":"47.5.0"}}"#;
        assert_eq!(
            forge_promo_version(promotions, "1.20.1").as_deref(),
            Some("47.4.0")
        );
        assert!(forge_promo_version(promotions, "1.21.1").is_none());
        assert!(forge_promo_version("not json", "1.20.1").is_none());
    }

    #[test]
    fn maven_metadata_picks_the_newest_stable_build_for_one_exact_version() {
        let metadata = "<metadata><versioning><versions>\
            <version>1.20.1-47.3.0</version>\
            <version>1.20.1-47.4.0</version>\
            <version>1.20.1-47.5.0-beta</version>\
            <version>1.20.11-1.0.0</version>\
            <version>1.20.2-48.0.0</version>\
            </versions></versioning></metadata>";
        assert_eq!(
            pick_prefixed_build(metadata, "1.20.1").as_deref(),
            Some("47.4.0")
        );
        assert_eq!(
            pick_prefixed_build(metadata, "1.20.2").as_deref(),
            Some("48.0.0")
        );
        assert!(pick_prefixed_build(metadata, "1.19.2").is_none());
        // The same `<minecraft>-<build>` list is the legacy NeoForge one.
        let legacy = "<metadata><versions>\
            <version>1.20.1-47.1.105</version>\
            <version>1.20.1-47.1.106</version>\
            </versions></metadata>";
        assert_eq!(
            pick_prefixed_build(legacy, "1.20.1").as_deref(),
            Some("47.1.106")
        );
    }

    #[test]
    fn neoforge_builds_follow_the_minecraft_line() {
        let metadata = "<metadata><versioning><versions>\
            <version>21.1.80</version>\
            <version>21.1.266</version>\
            <version>21.1.9</version>\
            <version>21.2.1</version>\
            <version>21.1.300-beta</version>\
            </versions></versioning></metadata>";
        assert_eq!(
            pick_neoforge_build(metadata, "1.21.1").as_deref(),
            Some("21.1.266")
        );
        assert!(pick_neoforge_build(metadata, "1.20.1").is_none());
        assert_eq!(neoforge_prefixes("1.21"), vec!["21.0.".to_owned()]);
        assert_eq!(neoforge_prefixes("1.20.1"), vec!["20.1.".to_owned()]);
        assert!(neoforge_prefixes("2.0").is_empty());
    }

    #[test]
    fn checksums_must_be_a_full_sha1() {
        assert_eq!(
            valid_sha1("D41D8CD98F00B204E9800998ECF8427E00000000\n").as_deref(),
            Some("d41d8cd98f00b204e9800998ecf8427e00000000")
        );
        assert!(valid_sha1("not-a-hash").is_none());
        assert!(valid_sha1("").is_none());
    }
}
