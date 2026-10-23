//! `.mrpack` and plain ZIP import with traversal/symlink defenses, bounded
//! extraction, parallel HTTPS downloads, and SHA-1/size verification.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
static TRANSACTION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum MrpackError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("download: {0}")]
    Download(#[from] super::downloader::DownloadError),
    #[error("invalid or unsupported Modrinth pack index")]
    InvalidIndex,
    #[error("archive has no modrinth.index.json, so its loader and version are unknown")]
    MissingIndex,
    #[error("archive path is unsafe: {0}")]
    UnsafePath(String),
    #[error("archive paths collide on the target filesystem: {0}")]
    PathCollision(String),
    #[error("archive contains a symbolic link: {0}")]
    Symlink(String),
    #[error("archive expands beyond the 2 GiB safety limit")]
    TooLarge,
    #[error("pack contains no downloadable files")]
    Empty,
    #[error(
        "installation failed ({cause}); rollback failed at {rollback_path}: {rollback}; recovery data is in {recovery_path}"
    )]
    Rollback {
        cause: String,
        rollback_path: String,
        rollback: String,
        recovery_path: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MrpackIndex {
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    pub game: String,
    #[serde(rename = "versionId")]
    pub version_id: String,
    #[serde(default)]
    pub name: String,
    pub dependencies: std::collections::HashMap<String, String>,
    pub files: Vec<MrpackFile>,
}

/// What the launcher needs before it can create an instance for a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSummary {
    pub name: String,
    pub version: String,
    pub game_version: String,
    /// `vanilla`, `fabric`, `forge`, `neoforge`, or `quilt`.
    pub loader: String,
    pub mod_files: usize,
    /// A square pack icon shipped inside the archive (`icon.png`, `pack.png`,
    /// or `logo.png` at the root or under `overrides/`). Remote packs usually
    /// carry the Modrinth project icon instead, which the caller fetches.
    pub icon: Option<Vec<u8>>,
}

/// Read a `.mrpack`'s index without installing anything. Used to name the new
/// instance and to pick the game version and loader the pack declares.
pub fn read_pack_summary(pack_path: &Path) -> Result<PackSummary, MrpackError> {
    let file = std::fs::File::open(pack_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let index = read_index(&mut archive)?.ok_or(MrpackError::MissingIndex)?;
    let loader = loader_from_dependencies(&index.dependencies);
    let game_version = index
        .dependencies
        .get("minecraft")
        .cloned()
        .unwrap_or_default();
    if game_version.is_empty() {
        return Err(MrpackError::InvalidIndex);
    }
    let name = if index.name.trim().is_empty() {
        // Older packs omit the display name; the version id is still useful.
        index.version_id.clone()
    } else {
        index.name.clone()
    };
    let icon = read_pack_icon(&mut archive);
    Ok(PackSummary {
        name,
        version: index.version_id,
        game_version,
        loader,
        mod_files: index
            .files
            .iter()
            .filter(|file| file.path.starts_with("mods/"))
            .count(),
        icon,
    })
}

/// Read the first recognised picture from the archive without trusting its
/// declared size. The caller stores it as 256 px instance artwork, so an 8 MiB
/// source ceiling is generous while still bounding a hostile pack.
fn read_pack_icon<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Option<Vec<u8>> {
    const ICON_BYTES: u64 = 8 * 1024 * 1024;
    const NAMES: [&str; 6] = [
        "icon.png", "pack.png", "logo.png", "icon.jpg", "pack.jpg", "logo.jpg",
    ];
    for entry_index in 0..archive.len() {
        let entry = archive.by_index(entry_index).ok()?;
        if entry.is_dir() || entry.size() > ICON_BYTES {
            continue;
        }
        let name = entry.name().to_ascii_lowercase();
        let relative = name
            .strip_prefix("overrides/")
            .or_else(|| name.strip_prefix("client-overrides/"))
            .unwrap_or(&name);
        if !NAMES.contains(&relative) {
            continue;
        }
        let mut bytes = Vec::with_capacity(entry.size().min(ICON_BYTES) as usize);
        entry.take(ICON_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > ICON_BYTES {
            continue;
        }
        return Some(bytes);
    }
    None
}

fn loader_from_dependencies(dependencies: &std::collections::HashMap<String, String>) -> String {
    const LOADERS: [(&str, &str); 4] = [
        ("fabric-loader", "fabric"),
        ("quilt-loader", "quilt"),
        ("neoforge", "neoforge"),
        ("forge", "forge"),
    ];
    LOADERS
        .iter()
        .find(|(key, _)| dependencies.contains_key(*key))
        .map(|(_, loader)| (*loader).to_owned())
        .unwrap_or_else(|| "vanilla".to_owned())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MrpackFile {
    pub path: String,
    pub hashes: MrpackHashes,
    pub downloads: Vec<String>,
    #[serde(rename = "fileSize")]
    pub file_size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MrpackHashes {
    pub sha1: String,
    #[serde(default)]
    pub sha512: String,
}

pub(crate) fn safe_relative_path(value: &str) -> Result<PathBuf, MrpackError> {
    if value.is_empty() || value.contains('\\') || value.contains('\0') {
        return Err(MrpackError::UnsafePath(value.to_owned()));
    }

    let mut path = PathBuf::new();
    for segment in value.split('/') {
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || has_windows_invalid_filename_char(segment)
            || segment.ends_with(' ')
            || segment.ends_with('.')
            || is_windows_device_name(segment)
        {
            return Err(MrpackError::UnsafePath(value.to_owned()));
        }
        path.push(segment);
    }
    Ok(path)
}

fn has_windows_invalid_filename_char(segment: &str) -> bool {
    segment.chars().any(|character| {
        character <= '\u{1f}' || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
    })
}

fn is_windows_device_name(segment: &str) -> bool {
    let stem = segment
        .split_once('.')
        .map_or(segment, |(stem, _)| stem)
        .trim_end_matches([' ', '.']);

    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return true;
    }

    let bytes = stem.as_bytes();
    let Some(prefix) = bytes.get(..3) else {
        return false;
    };
    let device_prefix = prefix.eq_ignore_ascii_case(b"COM") || prefix.eq_ignore_ascii_case(b"LPT");
    if !device_prefix {
        return false;
    }

    let suffix = &stem[3..];
    matches!(suffix.as_bytes(), [b'1'..=b'9']) || matches!(suffix, "¹" | "²" | "³")
}

fn portable_path_key(value: &str) -> String {
    value.chars().flat_map(char::to_uppercase).collect()
}

#[derive(Default)]
struct DestinationPaths {
    files: BTreeMap<String, String>,
    directories: BTreeSet<String>,
}

impl DestinationPaths {
    fn insert_file(&mut self, value: &str) -> Result<(), MrpackError> {
        safe_relative_path(value)?;
        let key = portable_path_key(value);
        if self.files.contains_key(&key) || self.directories.contains(&key) {
            return Err(MrpackError::PathCollision(value.to_owned()));
        }

        let mut parent = key.as_str();
        while let Some((ancestor, _)) = parent.rsplit_once('/') {
            if self.files.contains_key(ancestor) {
                return Err(MrpackError::PathCollision(value.to_owned()));
            }
            self.directories.insert(ancestor.to_owned());
            parent = ancestor;
        }

        let descendant_prefix = format!("{key}/");
        if self
            .files
            .range(descendant_prefix.clone()..)
            .next()
            .is_some_and(|(candidate, _)| candidate.starts_with(&descendant_prefix))
        {
            return Err(MrpackError::PathCollision(value.to_owned()));
        }

        self.files.insert(key, value.to_owned());
        Ok(())
    }

    fn insert_directory(&mut self, value: &str) -> Result<(), MrpackError> {
        safe_relative_path(value)?;
        let key = portable_path_key(value);
        if self.files.contains_key(&key) {
            return Err(MrpackError::PathCollision(value.to_owned()));
        }

        let mut parent = key.as_str();
        while let Some((ancestor, _)) = parent.rsplit_once('/') {
            if self.files.contains_key(ancestor) {
                return Err(MrpackError::PathCollision(value.to_owned()));
            }
            self.directories.insert(ancestor.to_owned());
            parent = ancestor;
        }
        self.directories.insert(key);
        Ok(())
    }

    fn conflicts_with_file(&self, key: &str) -> bool {
        if self.files.contains_key(key) {
            return true;
        }
        let mut parent = key;
        while let Some((ancestor, _)) = parent.rsplit_once('/') {
            if self.files.contains_key(ancestor) {
                return true;
            }
            parent = ancestor;
        }
        let descendant_prefix = format!("{key}/");
        self.files
            .range(descendant_prefix.clone()..)
            .next()
            .is_some_and(|(candidate, _)| candidate.starts_with(&descendant_prefix))
    }

    fn validate_overlay(&self, overlay: &Self) -> Result<(), MrpackError> {
        for (key, path) in &overlay.files {
            if let Some(index_path) = self.files.get(key) {
                if index_path == path {
                    continue;
                }
                return Err(MrpackError::PathCollision(path.clone()));
            }
            if self.conflicts_with_file(key) {
                return Err(MrpackError::PathCollision(path.clone()));
            }
        }
        Ok(())
    }
}

fn ensure_directory_tree_tracking(
    root: &Path,
    relative: &Path,
    mut created_dirs: Option<&mut Vec<PathBuf>>,
) -> Result<PathBuf, MrpackError> {
    std::fs::create_dir_all(root)?;
    let canonical_root = std::fs::canonicalize(root)?;
    let mut current = canonical_root.clone();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(MrpackError::UnsafePath(relative.display().to_string()));
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MrpackError::Symlink(current.display().to_string()));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(MrpackError::UnsafePath(current.display().to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
                if let Some(created_dirs) = created_dirs.as_deref_mut() {
                    created_dirs.push(current.clone());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    if !current.starts_with(&canonical_root) {
        return Err(MrpackError::UnsafePath(current.display().to_string()));
    }
    Ok(current)
}

fn safe_destination(root: &Path, relative: &str) -> Result<PathBuf, MrpackError> {
    safe_destination_tracking(root, relative, None)
}

fn safe_destination_tracking(
    root: &Path,
    relative: &str,
    created_dirs: Option<&mut Vec<PathBuf>>,
) -> Result<PathBuf, MrpackError> {
    let relative = safe_relative_path(relative)?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let parent_path = if parent.as_os_str().is_empty() {
        std::fs::create_dir_all(root)?;
        std::fs::canonicalize(root)?
    } else {
        ensure_directory_tree_tracking(root, parent, created_dirs)?
    };
    let name = relative
        .file_name()
        .ok_or_else(|| MrpackError::UnsafePath(relative.display().to_string()))?;
    let destination = parent_path.join(name);
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(MrpackError::Symlink(destination.display().to_string()));
        }
        Ok(metadata) if metadata.is_dir() => {
            return Err(MrpackError::UnsafePath(destination.display().to_string()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(destination)
}

struct InstallTransaction {
    root: PathBuf,
    staging_root: PathBuf,
    payload_root: PathBuf,
    backup_root: PathBuf,
    root_created: bool,
    committed: bool,
    preserve_staging: bool,
}

#[derive(Clone)]
struct CommitEntry {
    destination: PathBuf,
    backup: Option<PathBuf>,
    backup_moved: bool,
    installed: bool,
}

impl InstallTransaction {
    fn new(instance_root: &Path) -> Result<Self, MrpackError> {
        let root_created = match std::fs::symlink_metadata(instance_root) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MrpackError::Symlink(instance_root.display().to_string()));
            }
            Ok(metadata) if metadata.is_dir() => false,
            Ok(_) => return Err(MrpackError::UnsafePath(instance_root.display().to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(instance_root)?;
                true
            }
            Err(error) => return Err(error.into()),
        };
        let root = match std::fs::canonicalize(instance_root) {
            Ok(root) => root,
            Err(error) => {
                if root_created {
                    let _ = std::fs::remove_dir(instance_root);
                }
                return Err(error.into());
            }
        };

        let mut staging_root = None;
        for _ in 0..64 {
            let sequence = TRANSACTION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = root.join(format!(".kaelis-mrpack-{}-{sequence}", std::process::id()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    staging_root = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    if root_created {
                        let _ = std::fs::remove_dir(&root);
                    }
                    return Err(error.into());
                }
            }
        }
        let Some(staging_root) = staging_root else {
            if root_created {
                let _ = std::fs::remove_dir(&root);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a unique mrpack staging directory",
            )
            .into());
        };

        let payload_root = staging_root.join("payload");
        let backup_root = staging_root.join("backup");
        if let Err(error) =
            std::fs::create_dir(&payload_root).and_then(|()| std::fs::create_dir(&backup_root))
        {
            let _ = std::fs::remove_dir_all(&staging_root);
            if root_created {
                let _ = std::fs::remove_dir(&root);
            }
            return Err(error.into());
        }

        Ok(Self {
            root,
            staging_root,
            payload_root,
            backup_root,
            root_created,
            committed: false,
            preserve_staging: false,
        })
    }

    fn staged_destination(&self, relative: &str) -> Result<PathBuf, MrpackError> {
        safe_destination(&self.payload_root, relative)
    }

    fn commit(&mut self, paths: &[String]) -> Result<(), MrpackError> {
        self.commit_with(paths, |staged, destination| {
            std::fs::rename(staged, destination)
        })
    }

    fn commit_with<F>(&mut self, paths: &[String], mut install_staged: F) -> Result<(), MrpackError>
    where
        F: FnMut(&Path, &Path) -> std::io::Result<()>,
    {
        let mut created_dirs = Vec::new();
        let prepared = (|| {
            let mut seen = BTreeSet::new();
            let mut prepared = Vec::with_capacity(paths.len());
            for relative in paths {
                if !seen.insert(portable_path_key(relative)) {
                    return Err(MrpackError::PathCollision(relative.clone()));
                }
                let staged = self.staged_destination(relative)?;
                let staged_metadata = std::fs::symlink_metadata(&staged)?;
                if staged_metadata.file_type().is_symlink() {
                    return Err(MrpackError::Symlink(staged.display().to_string()));
                }
                if !staged_metadata.is_file() {
                    return Err(MrpackError::UnsafePath(staged.display().to_string()));
                }
                let destination =
                    safe_destination_tracking(&self.root, relative, Some(&mut created_dirs))?;
                prepared.push((staged, destination));
            }
            Ok(prepared)
        })();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                remove_empty_dirs(&created_dirs);
                return Err(error);
            }
        };

        let mut committed = Vec::with_capacity(prepared.len());
        for (index, (staged, destination)) in prepared.into_iter().enumerate() {
            let mut current = CommitEntry {
                destination: destination.clone(),
                backup: None,
                backup_moved: false,
                installed: false,
            };
            match std::fs::symlink_metadata(&destination) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let cause = MrpackError::Symlink(destination.display().to_string());
                    return Err(self.rollback_after_failure(
                        cause,
                        &committed,
                        Some(current),
                        &created_dirs,
                    ));
                }
                Ok(metadata) if metadata.is_dir() => {
                    let cause = MrpackError::UnsafePath(destination.display().to_string());
                    return Err(self.rollback_after_failure(
                        cause,
                        &committed,
                        Some(current),
                        &created_dirs,
                    ));
                }
                Ok(_) => {
                    let backup = self.backup_root.join(format!("{index:08}.bak"));
                    if let Err(error) = std::fs::rename(&destination, &backup) {
                        return Err(self.rollback_after_failure(
                            error.into(),
                            &committed,
                            None,
                            &created_dirs,
                        ));
                    }
                    current.backup = Some(backup);
                    current.backup_moved = true;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(self.rollback_after_failure(
                        error.into(),
                        &committed,
                        None,
                        &created_dirs,
                    ));
                }
            }

            if let Err(error) = install_staged(&staged, &destination) {
                return Err(self.rollback_after_failure(
                    error.into(),
                    &committed,
                    Some(current),
                    &created_dirs,
                ));
            }
            current.installed = true;
            committed.push(current);
        }

        self.committed = true;
        self.root_created = false;
        Ok(())
    }

    fn rollback_after_failure(
        &mut self,
        cause: MrpackError,
        committed: &[CommitEntry],
        current: Option<CommitEntry>,
        created_dirs: &[PathBuf],
    ) -> MrpackError {
        let mut entries = Vec::with_capacity(committed.len() + usize::from(current.is_some()));
        entries.extend_from_slice(committed);
        if let Some(current) = current {
            entries.push(current);
        }
        match rollback_entries(&entries, created_dirs) {
            Some((path, rollback)) => {
                self.preserve_staging = true;
                self.root_created = false;
                MrpackError::Rollback {
                    cause: cause.to_string(),
                    rollback_path: path.display().to_string(),
                    rollback,
                    recovery_path: self.staging_root.display().to_string(),
                }
            }
            None => cause,
        }
    }
}

impl Drop for InstallTransaction {
    fn drop(&mut self) {
        if !self.preserve_staging {
            let _ = std::fs::remove_dir_all(&self.staging_root);
            if !self.committed && self.root_created {
                let _ = std::fs::remove_dir(&self.root);
            }
        }
    }
}

fn remove_empty_dirs(directories: &[PathBuf]) {
    for directory in directories.iter().rev() {
        let _ = std::fs::remove_dir(directory);
    }
}

fn rollback_entries(
    entries: &[CommitEntry],
    created_dirs: &[PathBuf],
) -> Option<(PathBuf, String)> {
    let mut first_error = None;
    for entry in entries.iter().rev() {
        if entry.installed
            && let Err(error) = std::fs::remove_file(&entry.destination)
            && error.kind() != std::io::ErrorKind::NotFound
            && first_error.is_none()
        {
            first_error = Some((entry.destination.clone(), error.to_string()));
        }
        if entry.backup_moved
            && let Some(backup) = entry.backup.as_ref()
            && let Err(error) = std::fs::rename(backup, &entry.destination)
            && first_error.is_none()
        {
            first_error = Some((entry.destination.clone(), error.to_string()));
        }
    }
    for directory in created_dirs.iter().rev() {
        if let Err(error) = std::fs::remove_dir(directory)
            && error.kind() != std::io::ErrorKind::NotFound
            && first_error.is_none()
        {
            first_error = Some((directory.clone(), error.to_string()));
        }
    }
    first_error
}

fn is_symlink<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> bool {
    entry
        .unix_mode()
        .is_some_and(|mode| (mode & 0o170000) == 0o120000)
}

fn read_index<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<Option<MrpackIndex>, MrpackError> {
    let mut found = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.name() != "modrinth.index.json" {
            continue;
        }
        if found.is_some() {
            return Err(MrpackError::InvalidIndex);
        }
        if is_symlink(&entry) || entry.size() > MAX_INDEX_BYTES {
            return Err(MrpackError::InvalidIndex);
        }
        let mut raw = Vec::with_capacity(entry.size() as usize);
        entry.take(MAX_INDEX_BYTES + 1).read_to_end(&mut raw)?;
        if raw.len() as u64 > MAX_INDEX_BYTES {
            return Err(MrpackError::InvalidIndex);
        }
        let index: MrpackIndex = serde_json::from_slice(&raw)?;
        if index.format_version != 1 || index.game != "minecraft" {
            return Err(MrpackError::InvalidIndex);
        }
        found = Some(index);
    }
    Ok(found)
}

fn preflight_archive<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: Option<&MrpackIndex>,
) -> Result<(), MrpackError> {
    let mut expanded_bytes = 0_u64;
    let mut planned_download_bytes = 0_u64;
    let mut plain_paths = DestinationPaths::default();
    let mut indexed_paths = DestinationPaths::default();
    let mut override_paths = DestinationPaths::default();

    if let Some(index) = index {
        if index.files.is_empty() {
            return Err(MrpackError::Empty);
        }
        for pack_file in &index.files {
            if pack_file.downloads.is_empty() {
                return Err(MrpackError::Empty);
            }
            planned_download_bytes = planned_download_bytes.saturating_add(pack_file.file_size);
            if planned_download_bytes > MAX_ARCHIVE_BYTES {
                return Err(MrpackError::TooLarge);
            }
            indexed_paths.insert_file(&pack_file.path)?;
        }
    }

    for entry_index in 0..archive.len() {
        let entry = archive.by_index(entry_index)?;
        if entry.size() > MAX_ARCHIVE_BYTES {
            return Err(MrpackError::TooLarge);
        }
        expanded_bytes = expanded_bytes.saturating_add(entry.size());
        if expanded_bytes > MAX_ARCHIVE_BYTES {
            return Err(MrpackError::TooLarge);
        }
        if is_symlink(&entry) {
            return Err(MrpackError::Symlink(entry.name().to_owned()));
        }

        let name = entry.name();
        if index.is_none() {
            let relative = if entry.is_dir() {
                if name.is_empty() {
                    return Err(MrpackError::UnsafePath(name.to_owned()));
                }
                name.strip_suffix('/').unwrap_or(name)
            } else {
                name
            };
            if relative.is_empty() {
                return Err(MrpackError::UnsafePath(name.to_owned()));
            }
            if entry.is_dir() {
                plain_paths.insert_directory(relative)?;
            } else {
                plain_paths.insert_file(relative)?;
            }
            continue;
        }

        let relative = name
            .strip_prefix("overrides/")
            .or_else(|| name.strip_prefix("client-overrides/"));
        let Some(relative) = relative else {
            continue;
        };
        if entry.is_dir() {
            if relative.is_empty() {
                continue;
            }
            let directory = relative.strip_suffix('/').unwrap_or(relative);
            override_paths.insert_directory(directory)?;
        } else {
            override_paths.insert_file(relative)?;
        }
    }

    indexed_paths.validate_overlay(&override_paths)
}

/// Install a pack transactionally. Downloads and archive files are staged on
/// the instance filesystem and become visible only after every input succeeds.
pub async fn install_mrpack(
    client: &reqwest::Client,
    pack_path: &Path,
    instance_root: &Path,
) -> Result<usize, MrpackError> {
    install_mrpack_with_progress(client, pack_path, instance_root, |_| {}).await
}

/// Install a pack with stage lines for the caller's activity strip. The
/// callback is best-effort and never changes the import result.
pub async fn install_mrpack_with_progress<F>(
    client: &reqwest::Client,
    pack_path: &Path,
    instance_root: &Path,
    progress: F,
) -> Result<usize, MrpackError>
where
    F: Fn(&str) + Send + Sync + 'static,
{
    let progress = Arc::new(progress);
    progress("Reading the pack…");
    let file = std::fs::File::open(pack_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let index = read_index(&mut archive)?;
    preflight_archive(&mut archive, index.as_ref())?;

    let mut transaction = InstallTransaction::new(instance_root)?;
    let mut installed = 0_usize;
    let mut commit_paths = Vec::new();
    let mut commit_keys = BTreeSet::new();
    if let Some(index) = index {
        if index.files.is_empty() {
            return Err(MrpackError::Empty);
        }
        let mut tasks = Vec::with_capacity(index.files.len());
        for pack_file in &index.files {
            if pack_file.downloads.is_empty() {
                return Err(MrpackError::Empty);
            }
            let destination = transaction.staged_destination(&pack_file.path)?;
            let task = super::downloader::DownloadTask::new(
                &pack_file.downloads[0],
                destination,
                &pack_file.hashes.sha1,
            )
            .with_size(pack_file.file_size);
            tasks.push(task);
            commit_keys.insert(portable_path_key(&pack_file.path));
            commit_paths.push(pack_file.path.clone());
        }
        let download_progress = Arc::clone(&progress);
        installed +=
            super::downloader::download_all_with_progress(client, tasks, 8, move |done, total| {
                download_progress(&format!("Downloading {done} of {total} files…"));
            })
            .await?
            .len();
        progress("Applying pack overrides…");

        for entry_index in 0..archive.len() {
            let mut entry = archive.by_index(entry_index)?;
            if entry.is_dir() {
                continue;
            }
            let archive_name = entry.name().to_owned();
            let relative = archive_name
                .strip_prefix("overrides/")
                .or_else(|| archive_name.strip_prefix("client-overrides/"));
            let Some(relative) = relative else {
                continue;
            };
            let destination = transaction.staged_destination(relative)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(destination)?;
            // One buffered pass, no per-file fsync: the staged tree is renamed
            // into place as a batch, so a crash before the commit leaves no
            // half-applied pack, and syncing every override only added disk
            // latency to an operation that moves hundreds of small files.
            kaelis_core::stream::copy_stream(&mut entry, &mut output)?;
            installed += 1;
            if commit_keys.insert(portable_path_key(relative)) {
                commit_paths.push(relative.to_owned());
            }
        }
    } else {
        progress("Extracting the archive…");
        for entry_index in 0..archive.len() {
            let mut entry = archive.by_index(entry_index)?;
            if entry.is_dir() {
                continue;
            }
            let relative = entry.name().to_owned();
            let destination = transaction.staged_destination(&relative)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(destination)?;
            kaelis_core::stream::copy_stream(&mut entry, &mut output)?;
            installed += 1;
            commit_paths.push(relative);
        }
    }

    if commit_paths.is_empty() {
        return Err(MrpackError::Empty);
    }
    progress("Finalising…");
    transaction.commit(&commit_paths)?;
    progress("Finishing…");
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "kaelis-mrpack-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn summary_reports_the_pack_identity_before_installing_anything() {
        let directory = TestDirectory::new();
        let pack_path = directory.0.join("optimized.mrpack");
        let mut dependencies = std::collections::HashMap::new();
        dependencies.insert("minecraft".to_owned(), "1.21.1".to_owned());
        dependencies.insert("fabric-loader".to_owned(), "0.16.9".to_owned());
        let index = MrpackIndex {
            format_version: 1,
            game: "minecraft".to_owned(),
            version_id: "6.0.0".to_owned(),
            name: "Fabulously Optimized".to_owned(),
            dependencies,
            files: vec![
                MrpackFile {
                    path: "mods/sodium.jar".to_owned(),
                    hashes: MrpackHashes {
                        sha1: String::new(),
                        sha512: String::new(),
                    },
                    downloads: vec!["https://example.org/sodium.jar".to_owned()],
                    file_size: 1,
                },
                MrpackFile {
                    path: "resourcepacks/fresh.zip".to_owned(),
                    hashes: MrpackHashes {
                        sha1: String::new(),
                        sha512: String::new(),
                    },
                    downloads: vec!["https://example.org/fresh.zip".to_owned()],
                    file_size: 1,
                },
            ],
        };
        let file = std::fs::File::create(&pack_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(
                "modrinth.index.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer
            .write_all(&serde_json::to_vec(&index).unwrap())
            .unwrap();
        writer
            .start_file(
                "overrides/icon.png",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(b"pack-icon-bytes").unwrap();
        writer.finish().unwrap();

        let summary = read_pack_summary(&pack_path).unwrap();
        assert_eq!(summary.name, "Fabulously Optimized");
        assert_eq!(summary.version, "6.0.0");
        assert_eq!(summary.game_version, "1.21.1");
        assert_eq!(summary.loader, "fabric");
        assert_eq!(summary.mod_files, 1);
        assert_eq!(summary.icon.as_deref(), Some(b"pack-icon-bytes".as_slice()));
    }

    #[test]
    fn summary_refuses_an_archive_without_an_index() {
        let directory = TestDirectory::new();
        let pack_path = directory.0.join("plain.zip");
        let file = std::fs::File::create(&pack_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("readme.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"not a pack").unwrap();
        writer.finish().unwrap();
        assert!(matches!(
            read_pack_summary(&pack_path),
            Err(MrpackError::MissingIndex)
        ));
    }

    #[test]
    fn accepts_nested_relative_pack_paths() {
        let path = safe_relative_path("mods/performance/sodium.jar").unwrap();
        assert_eq!(
            path,
            PathBuf::from("mods").join("performance").join("sodium.jar")
        );
    }

    #[test]
    fn rejects_absolute_and_traversal_paths() {
        let unsafe_paths = [
            "",
            "../outside.jar",
            "mods/../../outside.jar",
            "/outside.jar",
            r"..\outside.jar",
            r"C:\outside.jar",
            "mods/file:stream.jar",
            "mods\0outside.jar",
            "mods//outside.jar",
            "mods/./outside.jar",
            "mods/sub/../outside.jar",
            "mods/",
        ];

        for path in unsafe_paths {
            assert!(
                matches!(safe_relative_path(path), Err(MrpackError::UnsafePath(_))),
                "path should be rejected: {path:?}"
            );
        }
    }

    #[test]
    fn rejects_windows_device_names_and_filename_aliases() {
        let unsafe_paths = [
            "mods/CON",
            "mods/prn.txt",
            "AUX.png",
            "nul",
            "mods/CONIN$",
            "mods/conout$.log",
            "mods/COM1.jar",
            "mods/lpt9.txt",
            "mods/COM¹.dll",
            "mods/lpt³.bin",
            "mods/trailing.",
            "mods/trailing ",
            "mods/bad<name>.jar",
            "mods/bad>name.jar",
            "mods/bad\"name.jar",
            "mods/bad|name.jar",
            "mods/bad?name.jar",
            "mods/bad*name.jar",
            "mods/control\u{001f}name.jar",
        ];

        for path in unsafe_paths {
            assert!(
                matches!(safe_relative_path(path), Err(MrpackError::UnsafePath(_))),
                "Windows-unsafe path should be rejected: {path:?}"
            );
        }
    }

    #[test]
    fn accepts_non_reserved_windows_filename_near_matches() {
        for path in [
            "mods/conifer.jar",
            "mods/COM0.jar",
            "mods/COM10.jar",
            "mods/LPT10.jar",
            "mods/COM¹⁰.jar",
            "mods/ordinary name.jar",
        ] {
            assert!(
                safe_relative_path(path).is_ok(),
                "valid near-match should be accepted: {path:?}"
            );
        }
    }

    #[test]
    fn rejects_case_collisions_and_file_directory_overlaps() {
        let mut case_collision = DestinationPaths::default();
        case_collision.insert_file("mods/Sodium.jar").unwrap();
        assert!(matches!(
            case_collision.insert_file("mods/sodium.jar"),
            Err(MrpackError::PathCollision(_))
        ));

        let mut unicode_case_collision = DestinationPaths::default();
        unicode_case_collision
            .insert_file(&format!("mods/{}odium.jar", '\u{03c3}'))
            .unwrap();
        assert!(matches!(
            unicode_case_collision.insert_file(&format!("mods/{}odium.jar", '\u{03c2}')),
            Err(MrpackError::PathCollision(_))
        ));

        let mut file_parent = DestinationPaths::default();
        file_parent.insert_file("config").unwrap();
        assert!(matches!(
            file_parent.insert_file("config/options.txt"),
            Err(MrpackError::PathCollision(_))
        ));

        let mut directory_parent = DestinationPaths::default();
        directory_parent.insert_file("config/options.txt").unwrap();
        assert!(matches!(
            directory_parent.insert_file("CONFIG"),
            Err(MrpackError::PathCollision(_))
        ));
    }

    #[test]
    fn permits_exact_override_and_rejects_aliases_of_index_files() {
        let mut indexed = DestinationPaths::default();
        indexed.insert_file("config/options.txt").unwrap();

        let mut exact_override = DestinationPaths::default();
        exact_override.insert_file("config/options.txt").unwrap();
        assert!(indexed.validate_overlay(&exact_override).is_ok());

        let mut case_alias = DestinationPaths::default();
        case_alias.insert_file("CONFIG/OPTIONS.TXT").unwrap();
        assert!(matches!(
            indexed.validate_overlay(&case_alias),
            Err(MrpackError::PathCollision(_))
        ));

        let mut nested_collision = DestinationPaths::default();
        nested_collision
            .insert_file("config/options.txt/child")
            .unwrap();
        assert!(matches!(
            indexed.validate_overlay(&nested_collision),
            Err(MrpackError::PathCollision(_))
        ));
    }

    #[tokio::test]
    async fn preflights_late_unsafe_zip_paths_before_writing_files() {
        let temp = TestDirectory::new();
        let archive_path = temp.0.join("invalid.zip");
        let instance_root = temp.0.join("instance");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        writer.start_file("good.txt", options).unwrap();
        writer.write_all(b"safe payload").unwrap();
        writer.start_file("../outside.txt", options).unwrap();
        writer.write_all(b"unsafe path").unwrap();
        writer.finish().unwrap();

        let result = install_mrpack(&reqwest::Client::new(), &archive_path, &instance_root).await;
        assert!(matches!(result, Err(MrpackError::UnsafePath(_))));
        assert!(!instance_root.exists());
        assert!(!temp.0.join("outside.txt").exists());
    }

    #[tokio::test]
    async fn imports_plain_zip_as_a_staged_batch() {
        let temp = TestDirectory::new();
        let archive_path = temp.0.join("valid.zip");
        let instance_root = temp.0.join("instance");
        std::fs::create_dir_all(instance_root.join("config")).unwrap();
        std::fs::write(instance_root.join("config/options.txt"), b"old settings").unwrap();

        let file = std::fs::File::create(&archive_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("mods/example.jar", options).unwrap();
        writer.write_all(b"mod payload").unwrap();
        writer.start_file("config/options.txt", options).unwrap();
        writer.write_all(b"new settings").unwrap();
        writer.finish().unwrap();

        let installed = install_mrpack(&reqwest::Client::new(), &archive_path, &instance_root)
            .await
            .unwrap();
        assert_eq!(installed, 2);
        assert_eq!(
            std::fs::read(instance_root.join("mods/example.jar")).unwrap(),
            b"mod payload"
        );
        assert_eq!(
            std::fs::read(instance_root.join("config/options.txt")).unwrap(),
            b"new settings"
        );
        assert_eq!(std::fs::read_dir(&instance_root).unwrap().count(), 2);
    }

    #[test]
    fn preflights_case_aliases_between_zip_entries() {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();

        writer.start_file("mods/Sodium.jar", options).unwrap();
        writer.write_all(b"first").unwrap();
        writer.start_file("mods/sodium.jar", options).unwrap();
        writer.write_all(b"second").unwrap();

        let bytes = writer.finish().unwrap().into_inner();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(matches!(
            preflight_archive(&mut archive, None),
            Err(MrpackError::PathCollision(_))
        ));
    }

    #[test]
    fn preflights_total_mrpack_download_size() {
        let index = MrpackIndex {
            format_version: 1,
            game: "minecraft".to_owned(),
            version_id: "bounded-pack".to_owned(),
            name: String::new(),
            dependencies: Default::default(),
            files: [MAX_ARCHIVE_BYTES, 1]
                .into_iter()
                .enumerate()
                .map(|(number, file_size)| MrpackFile {
                    path: format!("mods/file-{number}.jar"),
                    hashes: MrpackHashes {
                        sha1: String::new(),
                        sha512: String::new(),
                    },
                    downloads: vec![format!("https://example.org/file-{number}.jar")],
                    file_size,
                })
                .collect(),
        };
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file(
                "modrinth.index.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer
            .write_all(&serde_json::to_vec(&index).unwrap())
            .unwrap();

        let bytes = writer.finish().unwrap().into_inner();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(matches!(
            preflight_archive(&mut archive, Some(&index)),
            Err(MrpackError::TooLarge)
        ));
    }

    #[test]
    fn failed_batch_commit_restores_replaced_files_and_removes_new_outputs() {
        let temp = TestDirectory::new();
        let root = temp.0.join("instance");
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods/example.jar"), b"old-mod").unwrap();

        let mut transaction = InstallTransaction::new(&root).unwrap();
        let outputs = ["mods/example.jar", "config/settings.txt"].map(str::to_owned);
        std::fs::write(
            transaction.staged_destination(&outputs[0]).unwrap(),
            b"new-mod",
        )
        .unwrap();
        std::fs::write(
            transaction.staged_destination(&outputs[1]).unwrap(),
            b"settings",
        )
        .unwrap();

        let mut attempts = 0;
        let result = transaction.commit_with(&outputs, |staged, destination| {
            attempts += 1;
            if attempts == 2 {
                return Err(std::io::Error::other("injected commit failure"));
            }
            std::fs::rename(staged, destination)
        });
        assert!(matches!(
            result,
            Err(MrpackError::Io(ref error)) if error.to_string() == "injected commit failure"
        ));
        drop(transaction);

        assert_eq!(
            std::fs::read(root.join("mods/example.jar")).unwrap(),
            b"old-mod"
        );
        assert!(!root.join("config").exists());
        let entries = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(entries, 1, "staging and backup directories must be removed");
    }

    #[test]
    fn abandoned_import_transaction_removes_its_staging_root() {
        let temp = TestDirectory::new();
        let root = temp.0.join("new-instance");
        let transaction = InstallTransaction::new(&root).unwrap();
        std::fs::write(
            transaction.staged_destination("mods/example.jar").unwrap(),
            b"partial download",
        )
        .unwrap();
        drop(transaction);
        assert!(
            !root.exists(),
            "failed import must remove its new instance root"
        );
    }
}
