//! Persistent instance metadata. Runtime logs, tokens, and download staging
//! remain outside these JSON records.

use crate::format::human_bytes;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

static INSTANCE_FILE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum InstanceError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid instance id: {0}")]
    InvalidId(String),
    #[error("refusing to erase {0}: not a self-contained instance directory")]
    UnsafeRoot(String),
    #[error("instance folder is missing: {0}")]
    MissingRoot(String),
}

/// What [`Instance::erase`] actually removed, so the caller can report real
/// numbers instead of guessing at what the tree contained.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EraseReport {
    pub files: u64,
    pub bytes: u64,
    /// Whether an instance JSON record was removed (a dead instance may only
    /// have leftover metadata).
    pub metadata: bool,
}

/// Which parts of an instance a duplicate receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateOptions {
    /// `mods`, `resourcepacks`, and `shaderpacks`.
    pub content: bool,
    /// World saves. Off by default: they are the largest folder and are often
    /// deliberately kept separate when branching a pack.
    pub saves: bool,
    /// `config`, `options.txt`, key binds, servers, and every other file that
    /// is not content, a save, or disposable runtime output.
    pub settings: bool,
}

impl Default for DuplicateOptions {
    fn default() -> Self {
        Self {
            content: true,
            saves: false,
            settings: true,
        }
    }
}

/// What a duplicate wrote, so the caller can state real numbers.
#[derive(Debug, Clone)]
pub struct DuplicateReport {
    pub instance: Instance,
    pub files: u64,
    pub bytes: u64,
    /// Entries deliberately left out, such as runtime folders or links.
    pub skipped: Vec<String>,
}

impl DuplicateReport {
    pub fn summary(&self) -> String {
        format!(
            "Duplicated as {} ({}, {} file{})",
            self.instance.name,
            human_bytes(self.bytes),
            self.files,
            if self.files == 1 { "" } else { "s" }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub mc_version: String,
    pub loader: String,
    pub root: PathBuf,
    #[serde(default = "default_ram")]
    pub ram_mb: u32,
    /// Initial JVM heap in MiB. `None` keeps the launcher's 512 MiB default.
    #[serde(default)]
    pub min_ram_mb: Option<u32>,
    #[serde(default)]
    pub java_path: Option<PathBuf>,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    /// Extra arguments appended after the Mojang game arguments.
    #[serde(default)]
    pub game_args: Vec<String>,
    /// Extra process environment entries in `KEY=VALUE` form.
    #[serde(default)]
    pub env: Vec<String>,
    /// Free-form notes shown in the properties editor.
    #[serde(default)]
    pub notes: String,
    /// Optional grouping label used to organise the instance list.
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub fullscreen: bool,
    #[serde(default)]
    pub window_width: Option<u32>,
    #[serde(default)]
    pub window_height: Option<u32>,
    /// Join this `host` or `host:port` immediately after the client loads.
    #[serde(default)]
    pub quick_join: Option<String>,
    /// Shell command run before Java starts, with the instance root as cwd.
    #[serde(default)]
    pub pre_launch_command: Option<String>,
    /// Shell command run after the game exits.
    #[serde(default)]
    pub post_exit_command: Option<String>,
    /// Optional local image used as this instance's card artwork. The path is
    /// resolved by the GUI; the core crate never opens it.
    #[serde(default)]
    pub icon: Option<PathBuf>,
    #[serde(default)]
    pub last_played_unix: i64,
}

fn default_ram() -> u32 {
    4096
}

impl Instance {
    pub fn new(id: &str, name: &str, mc_version: &str, loader: &str, root: PathBuf) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            mc_version: mc_version.to_owned(),
            loader: loader.to_owned(),
            root,
            ram_mb: default_ram(),
            min_ram_mb: None,
            java_path: None,
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            env: Vec::new(),
            notes: String::new(),
            group: String::new(),
            fullscreen: false,
            window_width: None,
            window_height: None,
            quick_join: None,
            pre_launch_command: None,
            post_exit_command: None,
            icon: None,
            last_played_unix: 0,
        }
    }

    /// Reject path separators, dot segments, and names that are unsafe on
    /// Windows before using an id as part of a metadata path.
    fn validate_id(id: &str) -> Result<(), InstanceError> {
        let valid = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            });
        if valid {
            Ok(())
        } else {
            Err(InstanceError::InvalidId(id.to_owned()))
        }
    }

    /// Standard instances directory (override with `KAELIS_DATA_DIR`).
    pub fn instances_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("KAELIS_DATA_DIR") {
            return PathBuf::from(dir).join("instances");
        }
        directories::ProjectDirs::from("gg", "kaelis", "Kaelis")
            .map(|dirs| dirs.data_dir().join("instances"))
            .unwrap_or_else(|| PathBuf::from("instances"))
    }

    pub fn save(&self) -> Result<PathBuf, InstanceError> {
        self.save_in(&Self::instances_dir())
    }

    /// Write the record into an explicit instances directory. Importing an
    /// archive needs this to write into a staging directory before the record
    /// joins the live list, and tests use it to stay off the real data root.
    pub fn save_in(&self, dir: &Path) -> Result<PathBuf, InstanceError> {
        Self::validate_id(&self.id)?;
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", self.id));
        let sequence = INSTANCE_FILE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp_path = dir.join(format!(
            ".{}.{}.{}.tmp",
            self.id,
            std::process::id(),
            sequence
        ));
        let bytes = serde_json::to_vec_pretty(self)?;
        let write_result = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok::<(), std::io::Error>(())
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&temp_path);
            return Err(InstanceError::Io(error));
        }
        if let Err(error) = std::fs::rename(&temp_path, &path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(InstanceError::Io(error));
        }
        Ok(path)
    }

    pub fn load(id: &str) -> Result<Self, InstanceError> {
        Self::validate_id(id)?;
        let path = Self::instances_dir().join(format!("{id}.json"));
        let raw = std::fs::read(path)?;
        Ok(serde_json::from_slice(&raw)?)
    }

    pub fn load_all() -> Result<Vec<Self>, InstanceError> {
        Self::load_all_in(&Self::instances_dir())
    }

    pub fn load_all_in(dir: &Path) -> Result<Vec<Self>, InstanceError> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        // One unreadable or hand-edited record must not hide every other
        // instance: a single bad file used to fail the whole scan, leaving the
        // UI with no rows while those ids still occupied the disk.
        let mut instances: Vec<Self> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for entry in std::fs::read_dir(dir)?.filter_map(Result::ok) {
            if !entry.path().extension().is_some_and(|ext| ext == "json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(instance) = serde_json::from_slice::<Self>(&bytes) else {
                continue;
            };
            if Self::validate_id(&instance.id).is_err() {
                continue;
            }
            if seen.insert(instance.id.clone()) {
                instances.push(instance);
            }
        }
        instances.sort_by(|left, right| {
            right
                .last_played_unix
                .cmp(&left.last_played_unix)
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(instances)
    }

    /// Delete every byte this instance owns: the whole instance tree, its JSON
    /// record, and any temp record left behind by an interrupted save.
    ///
    /// The guard is deliberately strict, because a hand-edited or corrupted
    /// record must never be able to erase anything except one directory that is
    /// unambiguously an instance root. The root must be named exactly after the
    /// instance id, be at least three components deep, not contain the instances
    /// directory, and (when it exists) be a real directory rather than a link.
    pub fn erase(&self) -> Result<EraseReport, InstanceError> {
        self.erase_in(&Self::instances_dir())
    }

    fn erase_in(&self, instances_dir: &Path) -> Result<EraseReport, InstanceError> {
        Self::validate_id(&self.id)?;
        let root = self.root.as_path();
        let named_for_instance = root
            .file_name()
            .is_some_and(|name| name == self.id.as_str());
        let deep_enough = root.components().count() >= 3;
        let holds_records = instances_dir.starts_with(root);
        if !named_for_instance || !deep_enough || holds_records {
            return Err(InstanceError::UnsafeRoot(root.display().to_string()));
        }
        let mut report = EraseReport::default();
        match std::fs::symlink_metadata(root) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(InstanceError::UnsafeRoot(root.display().to_string()));
            }
            Ok(_) => {
                let (files, bytes) = tree_usage(root);
                std::fs::remove_dir_all(root)?;
                report.files = files;
                report.bytes = bytes;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(InstanceError::Io(error)),
        }
        let record = instances_dir.join(format!("{}.json", self.id));
        match std::fs::remove_file(&record) {
            Ok(()) => report.metadata = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(InstanceError::Io(error)),
        }
        remove_stale_records(instances_dir, &self.id);
        Ok(report)
    }

    /// Copy this instance into a new one in the standard instances directory.
    pub fn duplicate(&self, options: DuplicateOptions) -> Result<DuplicateReport, InstanceError> {
        self.duplicate_in(&Self::instances_dir(), options)
    }

    /// Copy the instance tree and write a fresh record beside it.
    ///
    /// Runtime output (logs, crash reports, extracted natives, launcher state)
    /// is never copied: it is disposable and a copy would start with a
    /// misleading history. Links and junctions are skipped rather than followed,
    /// so a copy can never reach outside the source tree. A failed copy removes
    /// its partial destination instead of leaving a half instance behind.
    pub fn duplicate_in(
        &self,
        instances_dir: &Path,
        options: DuplicateOptions,
    ) -> Result<DuplicateReport, InstanceError> {
        Self::validate_id(&self.id)?;
        if !self.root.is_dir() {
            return Err(InstanceError::MissingRoot(self.root.display().to_string()));
        }
        // A junctioned instance root would make every read escape the tree, so
        // refuse the copy instead of materialising someone else's folder.
        if is_reparse_point(&self.root) {
            return Err(InstanceError::UnsafeRoot(self.root.display().to_string()));
        }
        let (id, name) = self.copy_identity(instances_dir);
        // A first run may not have created the records directory yet, and the
        // copy is still useful before any instance has been saved there.
        std::fs::create_dir_all(instances_dir)?;
        let destination = instances_dir.join(&id);
        if let Err(error) = std::fs::create_dir(&destination) {
            return Err(InstanceError::Io(error));
        }

        let mut files = 0u64;
        let mut bytes = 0u64;
        let mut skipped = Vec::new();
        let copied = copy_tree(
            &self.root,
            &destination,
            options,
            &mut files,
            &mut bytes,
            &mut skipped,
        );
        if let Err(error) = copied {
            let _ = std::fs::remove_dir_all(&destination);
            return Err(error);
        }

        let mut duplicate = self.clone();
        duplicate.id = id;
        duplicate.name = name;
        duplicate.root = destination.clone();
        duplicate.last_played_unix = 0;
        if let Err(error) = duplicate.save_in(instances_dir) {
            let _ = std::fs::remove_dir_all(duplicate.root.clone());
            return Err(error);
        }
        skipped.sort();
        Ok(DuplicateReport {
            instance: duplicate,
            files,
            bytes,
            skipped,
        })
    }

    /// Pick an id and display name no existing record claims.
    fn copy_identity(&self, instances_dir: &Path) -> (String, String) {
        let existing = Self::load_all_in(instances_dir).unwrap_or_default();
        let ids: std::collections::HashSet<&str> = existing
            .iter()
            .map(|instance| instance.id.as_str())
            .collect();
        let names: std::collections::HashSet<String> = existing
            .iter()
            .map(|instance| instance.name.to_lowercase())
            .collect();
        let mut id_number = 1u32;
        let id = loop {
            let suffix = match id_number {
                1 => "-copy".to_owned(),
                count => format!("-copy-{count}"),
            };
            let keep = 64usize.saturating_sub(suffix.len());
            let base = self.id.get(..self.id.len().min(keep)).unwrap_or(&self.id);
            let candidate = format!("{base}{suffix}");
            if !ids.contains(candidate.as_str()) && !instances_dir.join(&candidate).exists() {
                break candidate;
            }
            id_number += 1;
        };
        let base_name = self.name.trim();
        let base_name = if base_name.is_empty() {
            "Instance"
        } else {
            base_name
        };
        let name = (1u32..)
            .map(|count| match count {
                1 => format!("{base_name} (copy)"),
                count => format!("{base_name} (copy {count})"),
            })
            .find(|candidate| !names.contains(&candidate.to_lowercase()))
            .unwrap_or_else(|| format!("{base_name} (copy)"));
        (id, name)
    }
}

/// Top-level folders whose contents are regenerated on every launch.
const RUNTIME_DIRS: [&str; 5] = ["logs", "crash-reports", "natives", "temp", ".kaelis"];
/// Folders that hold downloaded game content rather than user settings.
const CONTENT_DIRS: [&str; 4] = ["mods", "resourcepacks", "shaderpacks", "datapacks"];

/// Copy `source` into the already-created `destination`, honouring the
/// duplicate options and reporting what it skipped.
fn copy_tree(
    source: &Path,
    destination: &Path,
    options: DuplicateOptions,
    files: &mut u64,
    bytes: &mut u64,
    skipped: &mut Vec<String>,
) -> Result<(), InstanceError> {
    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((from, to)) = pending.pop() {
        for entry in std::fs::read_dir(&from)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            let target = to.join(&name);
            let file_type = entry.file_type()?;
            let relative = entry
                .path()
                .strip_prefix(source)
                .unwrap_or(&entry.path())
                .to_path_buf();
            let top_level = relative.components().count() == 1;
            if file_type.is_symlink() || is_reparse_point(&entry.path()) {
                skipped.push(relative.display().to_string());
                continue;
            }
            if file_type.is_dir() {
                // Windows paths are case-insensitive, so `Mods` and `mods` are
                // the same folder and must get the same bucket.
                let lower = name_lossy.to_ascii_lowercase();
                let bucket = if top_level && RUNTIME_DIRS.contains(&lower.as_str()) {
                    None
                } else if top_level && CONTENT_DIRS.contains(&lower.as_str()) {
                    options.content.then_some(())
                } else if top_level && lower == "saves" {
                    options.saves.then_some(())
                } else {
                    options.settings.then_some(())
                };
                if bucket.is_none() {
                    skipped.push(relative.display().to_string());
                    continue;
                }
                std::fs::create_dir(&target)?;
                pending.push((entry.path(), target));
                continue;
            }
            if !file_type.is_file() || !top_level_file_allowed(top_level, options) {
                skipped.push(relative.display().to_string());
                continue;
            }
            *bytes = bytes.saturating_add(std::fs::copy(entry.path(), &target)?);
            *files += 1;
        }
    }
    Ok(())
}

/// Root-level files belong to the settings bucket; anything nested was already
/// admitted by its parent folder.
fn top_level_file_allowed(top_level: bool, options: DuplicateOptions) -> bool {
    !top_level || options.settings
}

/// Windows junctions are directories with a reparse point; `is_symlink` alone
/// does not catch them, and following one would copy a foreign tree.
#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_reparse_point(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

/// Best-effort sweep of `.id.pid.seq.tmp` records written by a save that was
/// interrupted before its rename. These hold full instance metadata.
fn remove_stale_records(instances_dir: &Path, id: &str) {
    let Ok(entries) = std::fs::read_dir(instances_dir) else {
        return;
    };
    let prefix = format!(".{id}.");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".tmp") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// File count and byte total beneath `root`, without following links out of the
/// tree so a stray junction cannot inflate the report.
fn tree_usage(root: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            files += 1;
            if let Ok(metadata) = entry.metadata() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    (files, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_safe_instance_ids() {
        for id in ["vanilla-1-21", "fabric_1-21", "0"] {
            assert!(
                Instance::validate_id(id).is_ok(),
                "unexpected rejection: {id}"
            );
        }
    }

    #[test]
    fn rejects_ids_that_could_escape_the_instances_directory() {
        for id in [
            "",
            "../outside",
            "A",
            "with space",
            "with/slash",
            "with\\slash",
        ] {
            assert!(matches!(
                Instance::validate_id(id),
                Err(InstanceError::InvalidId(_))
            ));
        }
        assert!(Instance::validate_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn java_path_round_trips_and_old_metadata_defaults_to_path_lookup() {
        let mut instance = Instance::new(
            "vanilla-1-21",
            "Vanilla 1.21",
            "1.21",
            "vanilla",
            PathBuf::from("instance"),
        );
        instance.java_path = Some(PathBuf::from("runtime/bin/java.exe"));
        let encoded = serde_json::to_vec(&instance).unwrap();
        let decoded: Instance = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.java_path, instance.java_path);

        let mut legacy = serde_json::to_value(instance).unwrap();
        legacy.as_object_mut().unwrap().remove("java_path");
        let decoded: Instance = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.java_path, None);
    }

    #[test]
    fn artwork_round_trips_and_legacy_metadata_defaults() {
        let mut instance = Instance::new(
            "vanilla-1-21",
            "Vanilla 1.21",
            "1.21",
            "vanilla",
            PathBuf::from("instance"),
        );
        instance.icon = Some(PathBuf::from("C:/art/diamond.png"));
        instance.game_args = vec!["--fullscreen".to_owned()];
        instance.env = vec!["KAELIS_TEST=1".to_owned()];
        instance.min_ram_mb = Some(1024);
        instance.notes = "A test note".to_owned();
        instance.group = "Favourites".to_owned();
        instance.window_width = Some(1280);
        instance.window_height = Some(720);
        instance.quick_join = Some("play.example.net:25565".to_owned());
        instance.pre_launch_command = Some("echo before".to_owned());
        instance.post_exit_command = Some("echo after".to_owned());
        let encoded = serde_json::to_vec(&instance).unwrap();
        let decoded: Instance = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.icon, instance.icon);
        assert_eq!(decoded.game_args, instance.game_args);
        assert_eq!(decoded.env, instance.env);
        assert_eq!(decoded.min_ram_mb, Some(1024));
        assert_eq!(decoded.notes, instance.notes);
        assert_eq!(decoded.group, instance.group);
        assert_eq!(decoded.fullscreen, instance.fullscreen);
        assert_eq!(decoded.window_width, Some(1280));
        assert_eq!(decoded.window_height, Some(720));
        assert_eq!(decoded.quick_join, instance.quick_join);
        assert_eq!(decoded.pre_launch_command, instance.pre_launch_command);
        assert_eq!(decoded.post_exit_command, instance.post_exit_command);

        let mut legacy = serde_json::to_value(instance).unwrap();
        let object = legacy.as_object_mut().unwrap();
        object.remove("icon");
        object.remove("game_args");
        object.remove("env");
        for field in [
            "min_ram_mb",
            "notes",
            "group",
            "fullscreen",
            "window_width",
            "window_height",
            "quick_join",
            "pre_launch_command",
            "post_exit_command",
        ] {
            object.remove(field);
        }
        let decoded: Instance = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.icon, None);
        assert!(decoded.game_args.is_empty());
        assert!(decoded.env.is_empty());
        assert_eq!(decoded.min_ram_mb, None);
        assert!(decoded.notes.is_empty());
        assert!(decoded.group.is_empty());
        assert!(!decoded.fullscreen);
        assert_eq!(decoded.window_width, None);
        assert_eq!(decoded.window_height, None);
        assert_eq!(decoded.quick_join, None);
        assert_eq!(decoded.pre_launch_command, None);
        assert_eq!(decoded.post_exit_command, None);
    }

    use std::sync::atomic::{AtomicU64, Ordering};

    /// Sandbox for erase tests: never touches the real instances directory
    /// because `erase_in` takes the records path explicitly.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(tag: &str) -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let base = std::env::temp_dir().join(format!(
                "kaelis-erase-{}-{}-{}",
                std::process::id(),
                tag,
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&base).unwrap();
            Self(base)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn erase_removes_the_tree_and_every_record_of_one_instance() {
        let temp = TempTree::new("tree");
        let instances = temp.path().join("instances");
        let root = instances.join("fabric-1-21");
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods").join("sodium.jar"), vec![7u8; 4096]).unwrap();
        std::fs::write(root.join("options.txt"), "fov:90\n").unwrap();
        std::fs::write(instances.join("fabric-1-21.json"), "{}").unwrap();
        std::fs::write(instances.join(".fabric-1-21.999.1.tmp"), "{}").unwrap();
        std::fs::write(instances.join("other.json"), "{}").unwrap();
        let instance = Instance::new("fabric-1-21", "Fabric 1.21", "1.21", "fabric", root.clone());

        let report = instance.erase_in(&instances).unwrap();

        assert_eq!(report.files, 2);
        assert_eq!(report.bytes, 4096 + "fov:90\n".len() as u64);
        assert!(report.metadata);
        assert!(!root.exists(), "the instance tree must be gone");
        assert!(!instances.join("fabric-1-21.json").exists());
        assert!(!instances.join(".fabric-1-21.999.1.tmp").exists());
        assert!(
            instances.exists(),
            "records of other instances must survive"
        );
        assert!(instances.join("other.json").exists());
    }

    #[test]
    fn erase_reports_zero_files_for_metadata_only_instances() {
        let temp = TempTree::new("dead");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&instances).unwrap();
        let root = instances.join("vanilla-1-20");
        std::fs::write(instances.join("vanilla-1-20.json"), "{}").unwrap();
        let instance = Instance::new("vanilla-1-20", "Vanilla", "1.20", "vanilla", root);

        let report = instance.erase_in(&instances).unwrap();

        assert_eq!(report.files, 0);
        assert_eq!(report.bytes, 0);
        assert!(report.metadata);
        assert!(!instances.join("vanilla-1-20.json").exists());
    }

    #[test]
    fn erase_refuses_a_root_not_named_after_the_instance() {
        let temp = TempTree::new("name");
        let instances = temp.path().join("instances");
        let foreign = temp.path().join("documents");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("keep.txt"), "keep").unwrap();
        let instance = Instance::new("fabric-1-21", "Fabric", "1.21", "fabric", foreign.clone());

        let error = instance.erase_in(&instances).unwrap_err();

        assert!(matches!(error, InstanceError::UnsafeRoot(_)), "{error}");
        assert!(foreign.join("keep.txt").exists());
    }

    #[test]
    fn erase_refuses_a_root_that_contains_the_instances_directory() {
        let temp = TempTree::new("ancestor");
        let root = temp.path().join("instances");
        let records = root.join("records");
        std::fs::create_dir_all(&records).unwrap();
        std::fs::write(root.join("keep.txt"), "keep").unwrap();
        let instance = Instance::new("instances", "Root", "1.21", "vanilla", root.clone());

        let error = instance.erase_in(&records).unwrap_err();

        assert!(matches!(error, InstanceError::UnsafeRoot(_)), "{error}");
        assert!(root.join("keep.txt").exists());
    }

    #[test]
    fn erase_refuses_paths_without_a_named_leaf() {
        let instance = Instance::new("abc", "Root", "1.21", "vanilla", PathBuf::from("/"));

        let error = instance.erase_in(Path::new("/records")).unwrap_err();

        assert!(matches!(error, InstanceError::UnsafeRoot(_)), "{error}");
    }

    #[test]
    fn load_all_skips_a_corrupt_record_instead_of_hiding_every_instance() {
        let temp = TempTree::new("scan");
        let root = temp.path().join("vanilla-1-21");
        let instance = Instance::new(
            "vanilla-1-21",
            "Vanilla 1.21",
            "1.21",
            "vanilla",
            root.clone(),
        );
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            temp.path().join("vanilla-1-21.json"),
            serde_json::to_vec(&instance).unwrap(),
        )
        .unwrap();
        std::fs::write(temp.path().join("broken.json"), b"{ not json").unwrap();
        std::fs::write(
            temp.path().join("unsafe.json"),
            br#"{"id":"../escape","name":"Escape","mc_version":"1.21","loader":"vanilla","root":"x"}"#,
        )
        .unwrap();

        let loaded = Instance::load_all_in(temp.path()).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "vanilla-1-21");
    }

    #[test]
    fn erase_refuses_invalid_ids_before_touching_the_disk() {
        let temp = TempTree::new("id");
        let root = temp.path().join("keep-me");
        std::fs::create_dir_all(&root).unwrap();
        let instance = Instance::new("../keep-me", "Escape", "1.21", "vanilla", root.clone());

        let error = instance
            .erase_in(&temp.path().join("instances"))
            .unwrap_err();

        assert!(matches!(error, InstanceError::InvalidId(_)), "{error}");
        assert!(root.exists());
    }

    fn seeded_instance(temp: &TempTree) -> (PathBuf, Instance) {
        let instances = temp.path().join("instances");
        let root = instances.join("fabric-1-21");
        for dir in ["mods", "config", "saves", "logs", "crash-reports"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("mods").join("sodium.jar"), vec![1u8; 2048]).unwrap();
        std::fs::write(root.join("config").join("sodium.json"), b"{}").unwrap();
        std::fs::write(root.join("saves").join("world.dat"), vec![2u8; 4096]).unwrap();
        std::fs::write(root.join("logs").join("latest.log"), "old").unwrap();
        std::fs::write(root.join("crash-reports").join("crash.txt"), "boom").unwrap();
        std::fs::write(root.join("options.txt"), "fov:90\n").unwrap();
        let mut instance =
            Instance::new("fabric-1-21", "Fabric 1.21", "1.21", "fabric", root.clone());
        instance.ram_mb = 6144;
        instance.notes = "keep me".to_owned();
        instance.save_in(&instances).unwrap();
        (instances, instance)
    }

    #[test]
    fn duplicate_copies_content_and_settings_but_not_runtime_output() {
        let temp = TempTree::new("copy");
        let (instances, instance) = seeded_instance(&temp);

        let report = instance
            .duplicate_in(&instances, DuplicateOptions::default())
            .unwrap();

        let copy = &report.instance;
        assert_eq!(copy.id, "fabric-1-21-copy");
        assert_eq!(copy.name, "Fabric 1.21 (copy)");
        assert_eq!(copy.ram_mb, 6144, "settings must travel with the copy");
        assert_eq!(copy.notes, "keep me");
        assert_eq!(copy.last_played_unix, 0);
        assert!(copy.root.join("mods").join("sodium.jar").exists());
        assert!(copy.root.join("config").join("sodium.json").exists());
        assert!(copy.root.join("options.txt").exists());
        assert!(!copy.root.join("saves").exists(), "saves default off");
        assert!(!copy.root.join("logs").exists());
        assert!(!copy.root.join("crash-reports").exists());
        assert_eq!(report.files, 3);
        assert_eq!(
            report.bytes,
            2048 + "{}".len() as u64 + "fov:90\n".len() as u64
        );
        assert!(report.skipped.iter().any(|entry| entry == "saves"));
        assert!(report.skipped.iter().any(|entry| entry == "logs"));
        assert!(
            Instance::load_all_in(&instances)
                .unwrap()
                .iter()
                .any(|saved| saved.id == "fabric-1-21-copy")
        );
        assert!(instance.root.join("saves").join("world.dat").exists());
    }

    #[test]
    fn duplicate_can_include_saves_and_picks_unique_names() {
        let temp = TempTree::new("copy-again");
        let (instances, instance) = seeded_instance(&temp);
        let all = DuplicateOptions {
            content: true,
            saves: true,
            settings: true,
        };
        instance.duplicate_in(&instances, all).unwrap();
        let second = instance.duplicate_in(&instances, all).unwrap();

        assert_eq!(second.instance.id, "fabric-1-21-copy-2");
        assert_eq!(second.instance.name, "Fabric 1.21 (copy 2)");
        assert!(
            second
                .instance
                .root
                .join("saves")
                .join("world.dat")
                .exists()
        );
        assert_eq!(Instance::load_all_in(&instances).unwrap().len(), 3);
    }

    #[test]
    fn duplicate_creates_the_records_directory_on_a_fresh_data_root() {
        let temp = TempTree::new("fresh");
        let instances = temp.path().join("instances");
        let root = instances.join("vanilla-1-21");
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods").join("sodium.jar"), b"jar").unwrap();
        let instance = Instance::new("vanilla-1-21", "Vanilla", "1.21", "vanilla", root.clone());

        let report = instance
            .duplicate_in(&instances, DuplicateOptions::default())
            .unwrap();

        assert_eq!(report.instance.id, "vanilla-1-21-copy");
        assert!(instances.join("vanilla-1-21-copy.json").exists());
        assert!(
            report
                .instance
                .root
                .join("mods")
                .join("sodium.jar")
                .exists()
        );
    }

    #[test]
    fn duplicate_refuses_a_missing_source_folder() {
        let temp = TempTree::new("missing");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&instances).unwrap();
        let instance = Instance::new("ghost", "Ghost", "1.21", "vanilla", instances.join("ghost"));

        let error = instance
            .duplicate_in(&instances, DuplicateOptions::default())
            .unwrap_err();

        assert!(matches!(error, InstanceError::MissingRoot(_)), "{error}");
        assert!(!instances.join("ghost-copy").exists());
    }
}
