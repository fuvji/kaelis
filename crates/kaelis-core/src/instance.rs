//! Persistent instance metadata. Runtime logs, tokens, and download staging
//! remain outside these JSON records.

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
    /// Accent palette index (`0..=5`). `None` derives a stable colour from the
    /// instance id, so older metadata keeps its existing look.
    #[serde(default)]
    pub icon_variant: Option<u8>,
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
            icon_variant: None,
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
        Self::validate_id(&self.id)?;
        let dir = Self::instances_dir();
        std::fs::create_dir_all(&dir)?;
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

    fn load_all_in(dir: &Path) -> Result<Vec<Self>, InstanceError> {
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
    fn icon_fields_round_trip_and_legacy_metadata_defaults_to_auto() {
        let mut instance = Instance::new(
            "vanilla-1-21",
            "Vanilla 1.21",
            "1.21",
            "vanilla",
            PathBuf::from("instance"),
        );
        instance.icon = Some(PathBuf::from("C:/art/diamond.png"));
        instance.icon_variant = Some(4);
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
        assert_eq!(decoded.icon_variant, Some(4));
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
        object.remove("icon_variant");
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
        assert_eq!(decoded.icon_variant, None);
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
}
