//! Portable instance archives.
//!
//! One `.kaelis.zip` carries an instance record plus its game folder, so an
//! instance can move between machines (and operating systems) without the
//! launcher database. Runtime folders are rebuilt locally and never travel.
//!
//! The reader is hostile-input safe: absolute paths, `..` segments, symlinks,
//! oversized entries, and archives without a Kaelis manifest are rejected
//! before anything is written outside the new instance directory.

use kaelis_core::instance::Instance;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use zip::write::SimpleFileOptions;

/// Archive layout version. Bumped only for incompatible changes.
pub const ARCHIVE_FORMAT: u32 = 1;
/// Record name at the archive root.
pub const MANIFEST_NAME: &str = "kaelis-instance.json";

/// Largest manifest that will be parsed into memory.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Largest single file accepted from an archive.
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Largest total unpacked size accepted from an archive.
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Upper bound on archive entries, so a zip bomb cannot exhaust the inode table.
const MAX_ENTRIES: usize = 200_000;

/// Folders rebuilt by the launcher or the game on the next launch. Shipping
/// them would multiply archive size for no benefit, and a stale runtime would
/// point at a Java binary that does not exist on the target machine. Loader
/// scratch (`.fabric`, `.quilt`, `.connector`) also stores absolute classpaths
/// from the exporting machine, which would be wrong on the target one.
const REBUILT_DIRS: [&str; 11] = [
    "runtime",
    "logs",
    "crash-reports",
    "natives",
    ".cache",
    ".mixin.out",
    ".fabric",
    ".quilt",
    ".connector",
    "debug",
    "temp",
];

/// Root-level diagnostics and OS noise that are never worth shipping.
const ROOT_JUNK: [&str; 4] = [".ds_store", "thumbs.db", "desktop.ini", "launcher_log.txt"];
const ROOT_JUNK_PREFIXES: [&str; 2] = ["hs_err_pid", "replay_pid"];

use kaelis_core::stream::{COPY_BUFFER, copy_stream};

fn is_root_junk(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ROOT_JUNK.contains(&name.as_str())
        || ROOT_JUNK_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

#[derive(Debug, Error)]
pub enum PortableError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("this archive is not a Kaelis instance (no {MANIFEST_NAME})")]
    MissingManifest,
    #[error("instance archive format {0} is newer than this launcher supports")]
    UnsupportedFormat(u32),
    #[error("instance folder is missing: {0}")]
    MissingFolder(String),
    #[error("refusing unsafe archive path: {0}")]
    UnsafePath(String),
    #[error("archive entry is too large: {0}")]
    EntryTooLarge(String),
    #[error("archive expands past the {0} byte limit")]
    ArchiveTooLarge(u64),
    #[error("archive has more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    #[error("invalid instance metadata: {0}")]
    Invalid(String),
    #[error("could not name the imported instance")]
    NoAvailableId,
}

/// What an export wrote, for an honest status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub files: u64,
    pub bytes: u64,
    /// Paths left out (rebuilt folders, links, unreadable entries).
    pub skipped: Vec<String>,
}

impl ExportReport {
    pub fn summary(&self) -> String {
        format!(
            "Exported {} file{} ({})",
            self.files,
            if self.files == 1 { "" } else { "s" },
            human_bytes(self.bytes)
        )
    }
}

/// What an import produced.
#[derive(Debug, Clone)]
pub struct ImportReport {
    pub instance: Instance,
    pub files: u64,
    pub bytes: u64,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        format!(
            "Imported {} · {} file{} · {}",
            self.instance.name,
            self.files,
            if self.files == 1 { "" } else { "s" },
            human_bytes(self.bytes)
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    instance: Instance,
}

/// Write `instance` and its game folder into `destination`.
///
/// The archive is built beside the destination and renamed into place, so an
/// interrupted export never leaves a half-written file at the chosen path.
pub fn export_instance(
    instance: &Instance,
    destination: &Path,
) -> Result<ExportReport, PortableError> {
    if !instance.root.is_dir() {
        return Err(PortableError::MissingFolder(
            instance.root.display().to_string(),
        ));
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = sibling_temp(destination);
    let result = write_archive(instance, destination, &temp);
    match result {
        Ok(report) => Ok(report),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn write_archive(
    instance: &Instance,
    destination: &Path,
    temp: &Path,
) -> Result<ExportReport, PortableError> {
    let file = std::fs::File::create(temp)?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::with_capacity(COPY_BUFFER, file));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let manifest = Manifest {
        format: ARCHIVE_FORMAT,
        instance: instance.clone(),
    };
    zip.start_file(MANIFEST_NAME, options)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;

    let mut report = ExportReport::default();
    // A stable order keeps two exports of an unchanged instance byte-identical,
    // which makes archives diffable and cache-friendly.
    let mut files = Vec::new();
    collect_files(
        &instance.root,
        &instance.root,
        &mut files,
        &mut report.skipped,
    );
    files.sort();
    for relative in files {
        let absolute = instance.root.join(&relative);
        let display = relative.display().to_string();
        if absolute == destination || absolute == temp {
            report.skipped.push(display);
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&absolute) else {
            report.skipped.push(display);
            continue;
        };
        if !metadata.is_file() {
            report.skipped.push(display);
            continue;
        }
        let Ok(mut source) = std::fs::File::open(&absolute) else {
            report.skipped.push(display);
            continue;
        };
        let name = relative.to_string_lossy().replace('\\', "/");
        zip.start_file(name, options_for(&relative))?;
        let copied = copy_stream(&mut source, &mut zip)?;
        report.files += 1;
        report.bytes = report.bytes.saturating_add(copied);
        if report.bytes > MAX_TOTAL_BYTES {
            return Err(PortableError::ArchiveTooLarge(MAX_TOTAL_BYTES));
        }
    }
    let mut writer = zip.finish()?;
    writer.flush()?;
    // Close the file before renaming so Windows can move it.
    drop(writer);
    if destination.exists() {
        std::fs::remove_file(destination)?;
    }
    std::fs::rename(temp, destination)?;
    Ok(report)
}

/// Store already-compressed files instead of deflating them again. Jars, game
/// assets, and screenshots are most of a typical instance, and deflating them
/// again costs CPU for almost no size win.
fn options_for(path: &Path) -> SimpleFileOptions {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let stored = matches!(
        extension.as_str(),
        "jar" | "zip" | "png" | "jpg" | "jpeg" | "webp" | "ogg" | "mp3" | "gz" | "xz" | "webm"
    );
    SimpleFileOptions::default()
        .compression_method(if stored {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        })
        .unix_permissions(0o644)
}

/// Walk `root`, collecting forward-slash relative file paths that belong in an
/// archive. Folders the launcher rebuilds are pruned at the top level.
fn collect_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>, skipped: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        skipped.push(display_relative(root, dir));
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            skipped.push(display_relative(root, &path));
            continue;
        };
        if file_type.is_symlink() {
            skipped.push(display_relative(root, &path));
            continue;
        }
        if file_type.is_dir() {
            if dir == root {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if REBUILT_DIRS.contains(&name.as_str()) {
                    skipped.push(display_relative(root, &path));
                    continue;
                }
            }
            collect_files(root, &path, files, skipped);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".tmp") || name.ends_with(".part") {
            skipped.push(display_relative(root, &path));
            continue;
        }
        // Diagnostics and OS litter live at the instance root only; a mod or
        // resource pack may legitimately be called `debug.log`.
        if dir == root && is_root_junk(&name) {
            skipped.push(display_relative(root, &path));
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.to_path_buf());
        }
    }
}

/// Unpack an exported instance into `instances_dir` under a free id.
pub fn import_instance(
    archive: &Path,
    instances_dir: &Path,
) -> Result<ImportReport, PortableError> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    if zip.len() > MAX_ENTRIES {
        return Err(PortableError::TooManyEntries);
    }
    let manifest = read_manifest(&mut zip)?;
    if manifest.format > ARCHIVE_FORMAT {
        return Err(PortableError::UnsupportedFormat(manifest.format));
    }
    let mut instance = manifest.instance;
    if instance.mc_version.trim().is_empty() || instance.name.trim().is_empty() {
        return Err(PortableError::Invalid(
            "the archive does not name a Minecraft version".to_owned(),
        ));
    }
    let id = unique_instance_id(&instance.id, instances_dir)?;
    instance.id = id.clone();
    instance.root = instances_dir.join(&id);
    instance.name = unique_instance_name(&instance.name, &id, instances_dir);
    if instance.name.trim().is_empty() {
        instance.name = id.clone();
    }
    std::fs::create_dir_all(&instance.root)?;
    let result = extract(&mut zip, &instance.root);
    let (files, bytes) = match result {
        Ok(counts) => counts,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&instance.root);
            return Err(error);
        }
    };
    if let Err(error) = instance.save_in(instances_dir) {
        let _ = std::fs::remove_dir_all(&instance.root);
        return Err(PortableError::Invalid(error.to_string()));
    }
    Ok(ImportReport {
        instance,
        files,
        bytes,
    })
}

fn read_manifest<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Result<Manifest, PortableError> {
    let mut entry = match zip.by_name(MANIFEST_NAME) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Err(PortableError::MissingManifest),
        Err(error) => return Err(PortableError::Zip(error)),
    };
    if entry.size() > MAX_MANIFEST_BYTES {
        return Err(PortableError::EntryTooLarge(MANIFEST_NAME.to_owned()));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    (&mut entry)
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn extract<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    root: &Path,
) -> Result<(u64, u64), PortableError> {
    let mut files = 0_u64;
    let mut total = 0_u64;
    // Archives hold hundreds of files per folder. Re-issuing `create_dir_all`
    // for every entry turns into a failing system call per file on Windows;
    // remembering the folders already made leaves one call per distinct folder.
    let mut directories = std::collections::HashSet::new();
    directories.insert(root.to_path_buf());
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let Some(name) = entry.enclosed_name() else {
            return Err(PortableError::UnsafePath(entry.name().to_string()));
        };
        if name.as_os_str().is_empty() || entry.name() == MANIFEST_NAME {
            continue;
        }
        let relative = name;
        // A record exported by this launcher never nests a second instance
        // manifest; refuse one rather than overwrite the live record later.
        if relative.components().count() == 1 && relative.to_string_lossy() == MANIFEST_NAME {
            continue;
        }
        if entry.is_dir() {
            ensure_dir(&safe_join(root, &relative)?, &mut directories)?;
            continue;
        }
        // Zip can carry Unix modes; a symlink entry would let an archive point
        // outside the instance after extraction.
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(PortableError::UnsafePath(entry.name().to_string()));
        }
        let declared = entry.size();
        if declared > MAX_ENTRY_BYTES {
            return Err(PortableError::EntryTooLarge(entry.name().to_string()));
        }
        total = total.saturating_add(declared);
        if total > MAX_TOTAL_BYTES {
            return Err(PortableError::ArchiveTooLarge(MAX_TOTAL_BYTES));
        }
        let destination = safe_join(root, &relative)?;
        if let Some(parent) = destination.parent() {
            ensure_dir(parent, &mut directories)?;
        }
        let mut output = std::fs::File::create(&destination)?;
        let written = copy_stream(&mut (&mut entry).take(MAX_ENTRY_BYTES + 1), &mut output)?;
        if written > MAX_ENTRY_BYTES {
            let _ = std::fs::remove_file(&destination);
            return Err(PortableError::EntryTooLarge(entry.name().to_string()));
        }
        files += 1;
    }
    Ok((files, total))
}

fn ensure_dir(
    path: &Path,
    created: &mut std::collections::HashSet<PathBuf>,
) -> std::io::Result<()> {
    if created.insert(path.to_path_buf()) {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Join a validated relative path below `root`.
fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf, PortableError> {
    let mut destination = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => destination.push(segment),
            Component::CurDir => {}
            _ => {
                return Err(PortableError::UnsafePath(relative.display().to_string()));
            }
        }
    }
    if !destination.starts_with(root) {
        return Err(PortableError::UnsafePath(relative.display().to_string()));
    }
    Ok(destination)
}

/// First free id in `instances_dir`, derived from the archive's id.
fn unique_instance_id(base: &str, instances_dir: &Path) -> Result<String, PortableError> {
    let base = sanitize_id(base);
    for attempt in 0..1000_u32 {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{}", attempt + 1)
        };
        let record = instances_dir.join(format!("{candidate}.json"));
        let folder = instances_dir.join(&candidate);
        if !record.exists() && !folder.exists() {
            return Ok(candidate);
        }
    }
    Err(PortableError::NoAvailableId)
}

fn sanitize_id(value: &str) -> String {
    let mut id: String = value
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    let id = id.trim_matches('-').to_owned();
    if id.is_empty() {
        return "imported-instance".to_owned();
    }
    id.chars().take(48).collect()
}

/// Keep the archive's display name, adding a suffix only when another record
/// already uses it, so a second import is still tellable apart.
fn unique_instance_name(name: &str, id: &str, instances_dir: &Path) -> String {
    let taken: Vec<String> = Instance::load_all_in(instances_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|instance| instance.name.to_ascii_lowercase())
        .collect();
    let name = name.trim();
    if !taken.contains(&name.to_ascii_lowercase()) {
        return name.to_owned();
    }
    for attempt in 2..1000_u32 {
        let candidate = format!("{name} ({attempt})");
        if !taken.contains(&candidate.to_ascii_lowercase()) {
            return candidate;
        }
    }
    format!("{name} ({id})")
}

fn sibling_temp(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("kaelis-instance.zip"));
    name.push(format!(
        ".{}.part",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    destination.with_file_name(name)
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub use kaelis_core::format::human_bytes;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kaelis-portable-{tag}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_instance(dir: &Path) -> Instance {
        let root = dir.join("instances").join("fabric-test");
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::create_dir_all(root.join("saves").join("world")).unwrap();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::write(root.join("mods").join("sodium.jar"), b"jar").unwrap();
        std::fs::write(root.join("saves").join("world").join("level.dat"), b"world").unwrap();
        std::fs::write(root.join("logs").join("latest.log"), b"noise").unwrap();
        let instance = Instance::new("fabric-test", "Test pack", "1.21.1", "fabric", root);
        instance.save_in(&dir.join("instances")).unwrap();
        instance
    }

    #[test]
    fn round_trips_an_instance_without_rebuilt_folders() {
        let dir = scratch("roundtrip");
        let instance = sample_instance(&dir);
        let archive = dir.join("test.kaelis.zip");
        let exported = export_instance(&instance, &archive).expect("export");
        assert_eq!(exported.files, 2);
        assert!(archive.is_file());

        let imported = import_instance(&archive, &dir.join("instances")).expect("import");
        assert_eq!(imported.instance.id, "fabric-test-2");
        assert_eq!(imported.instance.mc_version, "1.21.1");
        assert!("Test pack (2)" == imported.instance.name);
        assert_eq!(
            std::fs::read(imported.instance.root.join("mods").join("sodium.jar")).unwrap(),
            b"jar"
        );
        assert!(!imported.instance.root.join("logs").exists());
        // The imported record is live: load_all sees both instances.
        let all = Instance::load_all_in(&dir.join("instances")).unwrap();
        assert_eq!(all.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn leaves_loader_scratch_and_diagnostics_out_of_the_archive() {
        let dir = scratch("scratch");
        let instance = sample_instance(&dir);
        std::fs::create_dir_all(instance.root.join(".fabric")).unwrap();
        std::fs::write(
            instance.root.join(".fabric").join("remapClasspath.txt"),
            b"abs",
        )
        .unwrap();
        std::fs::write(instance.root.join("hs_err_pid42.log"), b"boom").unwrap();
        std::fs::write(instance.root.join("Thumbs.db"), b"noise").unwrap();
        // Only the root copy is litter; a content file may share the name.
        std::fs::write(instance.root.join("mods").join("Thumbs.db"), b"keep").unwrap();

        let archive = dir.join("scratch.kaelis.zip");
        let report = export_instance(&instance, &archive).expect("export");
        assert_eq!(report.files, 3, "skipped: {:?}", report.skipped);
        let imported = import_instance(&archive, &dir.join("instances")).expect("import");
        assert!(!imported.instance.root.join(".fabric").exists());
        assert!(!imported.instance.root.join("hs_err_pid42.log").exists());
        assert!(!imported.instance.root.join("Thumbs.db").exists());
        assert!(
            imported
                .instance
                .root
                .join("mods")
                .join("Thumbs.db")
                .is_file()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_an_archive_without_a_manifest() {
        let dir = scratch("nomanifest");
        let archive = dir.join("not-kaelis.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("readme.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"nothing here").unwrap();
        zip.finish().unwrap();
        assert!(matches!(
            import_instance(&archive, &dir.join("instances")),
            Err(PortableError::MissingManifest)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_traversal_and_leaves_no_directory_behind() {
        let dir = scratch("traversal");
        let archive = dir.join("evil.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let manifest = Manifest {
            format: ARCHIVE_FORMAT,
            instance: Instance::new("evil", "Evil", "1.21.1", "vanilla", dir.join("x")),
        };
        zip.start_file(MANIFEST_NAME, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.start_file("../escaped.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"nope").unwrap();
        zip.finish().unwrap();

        let instances = dir.join("instances");
        let result = import_instance(&archive, &instances);
        assert!(matches!(result, Err(PortableError::UnsafePath(_))));
        assert!(!instances.join("evil").exists());
        assert!(!dir.join("escaped.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_newer_archive_format() {
        let dir = scratch("format");
        let archive = dir.join("future.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let manifest = Manifest {
            format: ARCHIVE_FORMAT + 1,
            instance: Instance::new("future", "Future", "1.21.1", "vanilla", dir.join("x")),
        };
        zip.start_file(MANIFEST_NAME, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.finish().unwrap();
        assert!(matches!(
            import_instance(&archive, &dir.join("instances")),
            Err(PortableError::UnsupportedFormat(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
