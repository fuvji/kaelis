//! Local mod metadata reader used for dependency health and repair.
//!
//! Reads Fabric, Quilt, Forge, NeoForge, and legacy Forge metadata from a jar,
//! including the nested jars Fabric API uses to ship its modules. Every read is
//! size-capped so a hostile archive cannot pin the launcher in memory.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

const MAX_METADATA_BYTES: u64 = 256 * 1024;
const MAX_NESTED_JARS: usize = 96;
const MAX_NESTED_BYTES: u64 = 8 * 1024 * 1024;
/// Upper bound on cached jars. Instances rarely hold more than a few hundred
/// mods, so this only guards a long-running launcher against unbounded growth.
const MAX_CACHE_ENTRIES: usize = 4096;

struct CachedMetadata {
    len: u64,
    modified: Option<SystemTime>,
    metadata: ModMetadata,
}

/// Jars are re-scanned whenever the mods tab refreshes or an instance is
/// selected. Opening every archive (including Fabric API's nested modules)
/// each time is the expensive part, so metadata is memoised by path and
/// invalidated through the file's length and modification time.
static JAR_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedMetadata>>> = OnceLock::new();

#[derive(Debug, Default, Clone)]
pub struct ModMetadata {
    /// Normalized ids this jar provides, including nested Fabric API modules.
    pub ids: Vec<String>,
    /// Top-level ids only. Nested jar-in-jar modules deliberately do not
    /// appear here: Sodium and ukulib bundle Fabric API modules that are also
    /// present in Fabric API itself, and counting those as duplicate mods would
    /// disable a working jar.
    pub primary_ids: Vec<String>,
    /// Normalized required mod/module ids.
    pub requires: Vec<String>,
    /// Primary loader detected from this jar's metadata.
    pub loader: Option<&'static str>,
}

impl ModMetadata {
    fn absorb(&mut self, other: &ModMetadata) {
        self.ids.extend(other.ids.iter().cloned());
        self.requires.extend(other.requires.iter().cloned());
        // Nested metadata never owns the file's identity.
        if self.loader.is_none() {
            self.loader = other.loader;
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalMod {
    pub name: String,
    pub path: PathBuf,
    pub metadata: ModMetadata,
    pub disabled: bool,
}

impl LocalMod {
    /// Significant filename tokens used as a fallback when metadata is absent.
    pub fn tokens(&self) -> Vec<String> {
        let stem = self
            .name
            .to_ascii_lowercase()
            .trim_end_matches(".disabled")
            .trim_end_matches(".jar")
            .to_owned();
        stem.split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|token| token.len() >= 3)
            .map(str::to_owned)
            .collect()
    }
}

/// Cached variant of [`read_jar`]. The cache is process-wide and shared by the
/// GUI and CLI; a replaced jar is detected through its size and mtime.
pub fn read_jar_cached(path: &Path) -> ModMetadata {
    let Ok(file) = std::fs::metadata(path) else {
        return ModMetadata::default();
    };
    let len = file.len();
    let modified = file.modified().ok();
    let cache = JAR_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(entries) = cache.lock()
        && let Some(entry) = entries.get(path)
        && entry.len == len
        && entry.modified == modified
    {
        return entry.metadata.clone();
    }
    let metadata = read_jar(path);
    if let Ok(mut entries) = cache.lock() {
        if entries.len() >= MAX_CACHE_ENTRIES {
            entries.clear();
        }
        entries.insert(
            path.to_path_buf(),
            CachedMetadata {
                len,
                modified,
                metadata: metadata.clone(),
            },
        );
    }
    metadata
}

/// Read loader metadata, ids, and required ids from one jar.
pub fn read_jar(path: &Path) -> ModMetadata {
    let Ok(file) = std::fs::File::open(path) else {
        return ModMetadata::default();
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return ModMetadata::default();
    };
    let mut metadata = ModMetadata::default();
    for name in [
        "fabric.mod.json",
        "quilt.mod.json",
        "META-INF/mods.toml",
        "META-INF/neoforge.mods.toml",
        "mcmod.info",
    ] {
        let Some(text) = read_entry_text(&mut archive, name) else {
            continue;
        };
        collect_text_metadata(name, &text, &mut metadata);
    }
    read_nested_jars(&mut archive, &mut metadata);
    metadata.ids.retain(|id| !id.is_empty());
    metadata.ids.sort();
    metadata.ids.dedup();
    metadata.primary_ids.retain(|id| !id.is_empty());
    metadata.primary_ids.sort();
    metadata.primary_ids.dedup();
    metadata.requires.retain(|id| !id.is_empty());
    metadata.requires.sort();
    metadata.requires.dedup();
    metadata
}

/// Scan `mods/` for `.jar` and `.jar.disabled` files.
pub fn scan_dir(mods_dir: &Path) -> Vec<LocalMod> {
    let Ok(entries) = std::fs::read_dir(mods_dir) else {
        return Vec::new();
    };
    let mut mods = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            let disabled = lower.ends_with(".jar.disabled");
            if !disabled && !lower.ends_with(".jar") {
                return None;
            }
            Some(LocalMod {
                metadata: read_jar_cached(&path),
                name,
                path,
                disabled,
            })
        })
        .collect::<Vec<_>>();
    mods.sort_by(|left, right| left.name.cmp(&right.name));
    mods
}

/// Required ids that no enabled jar in `mods_dir` provides.
///
/// Loader-provided ids (`minecraft`, `java`, `fabricloader`, `forge`, …) are
/// ignored because installing a jar cannot satisfy them.
pub fn missing_required(mods_dir: &Path) -> Vec<String> {
    let mods = scan_dir(mods_dir);
    let mut provided = HashSet::new();
    let mut required = HashSet::new();
    for local in mods.iter().filter(|local| !local.disabled) {
        provided.extend(local.metadata.ids.iter().cloned());
        required.extend(local.metadata.requires.iter().cloned());
    }
    let mut missing = required
        .into_iter()
        .filter(|id| !is_builtin_id(id) && !provided.contains(id))
        .collect::<Vec<_>>();
    missing.sort();
    missing
}

/// Required Fabric module ids that no enabled jar provides.
///
/// Nested Fabric API jars are indexed, so a full Fabric API install reports no
/// missing modules even though its top-level metadata only declares `fabric`.
pub fn missing_fabric_modules(mods_dir: &Path) -> Vec<String> {
    missing_required(mods_dir)
        .into_iter()
        .filter(|id| id.starts_with("fabric"))
        .collect()
}

/// Detect the loader from enabled jar metadata, falling back to any jar.
pub fn detect_loader(mods_dir: &Path) -> Option<&'static str> {
    let mods = scan_dir(mods_dir);
    for only_enabled in [true, false] {
        let mut counts = [0_usize; 4];
        for local in mods.iter().filter(|local| only_enabled || !local.disabled) {
            let slot = match local.metadata.loader {
                Some("fabric") => 0,
                Some("quilt") => 1,
                Some("forge") => 2,
                Some("neoforge") => 3,
                _ => continue,
            };
            counts[slot] += 1;
        }
        let names = ["fabric", "quilt", "forge", "neoforge"];
        let mut best: Option<usize> = None;
        for (index, count) in counts.iter().enumerate() {
            if *count > 0 && best.map(|slot| counts[slot] < *count).unwrap_or(true) {
                best = Some(index);
            }
        }
        if let Some(index) = best {
            return Some(names[index]);
        }
    }
    None
}

fn read_entry_text<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    let entry = archive.by_name(name).ok()?;
    if entry.size() > MAX_METADATA_BYTES {
        return None;
    }
    let mut text = String::new();
    entry
        .take(MAX_METADATA_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// Fabric API ships each module as a nested jar whose `fabric.mod.json` declares
/// the module id. Without indexing those, every module looks missing.
fn read_nested_jars<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    metadata: &mut ModMetadata,
) {
    let names = archive
        .file_names()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".jar")
                && (lower.starts_with("meta-inf/jars/") || lower.starts_with("meta-inf/versions/"))
        })
        .take(MAX_NESTED_JARS)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for name in names {
        let Ok(entry) = archive.by_name(&name) else {
            continue;
        };
        if entry.size() > MAX_NESTED_BYTES {
            continue;
        }
        let mut bytes = Vec::new();
        if entry
            .take(MAX_NESTED_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
        {
            continue;
        }
        let Ok(mut nested) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
            continue;
        };
        for entry_name in ["fabric.mod.json", "quilt.mod.json"] {
            let Some(text) = read_entry_text(&mut nested, entry_name) else {
                continue;
            };
            let mut nested_metadata = ModMetadata::default();
            collect_text_metadata(entry_name, &text, &mut nested_metadata);
            metadata.absorb(&nested_metadata);
        }
    }
}

fn collect_text_metadata(name: &str, text: &str, metadata: &mut ModMetadata) {
    match name {
        "fabric.mod.json" => collect_fabric(text, metadata),
        "quilt.mod.json" => collect_quilt(text, metadata),
        "META-INF/mods.toml" => collect_forge_toml(text, "forge", metadata),
        "META-INF/neoforge.mods.toml" => collect_forge_toml(text, "neoforge", metadata),
        "mcmod.info" => collect_mcmod_info(text, metadata),
        _ => {}
    }
}

fn collect_fabric(text: &str, metadata: &mut ModMetadata) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    metadata.loader.get_or_insert("fabric");
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        metadata.ids.push(normalize(id));
        metadata.primary_ids.push(normalize(id));
    }
    push_json_id(&value, "name", &mut metadata.ids);
    if let Some(provides) = value.get("provides").and_then(Value::as_array) {
        for provided in provides.iter().filter_map(Value::as_str) {
            metadata.ids.push(normalize(provided));
        }
    }
    if let Some(depends) = value.get("depends").and_then(Value::as_object) {
        for required in depends.keys() {
            metadata.requires.push(normalize(required));
        }
    }
}

fn collect_quilt(text: &str, metadata: &mut ModMetadata) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let Some(loader) = value.get("quilt_loader") else {
        return;
    };
    metadata.loader.get_or_insert("quilt");
    if let Some(id) = loader.get("id").and_then(Value::as_str) {
        metadata.ids.push(normalize(id));
        metadata.primary_ids.push(normalize(id));
    }
    if let Some(meta) = loader.get("metadata") {
        push_json_id(meta, "name", &mut metadata.ids);
    }
    if let Some(provides) = loader.get("provides").and_then(Value::as_array) {
        for provided in provides {
            if let Some(id) = provided.as_str() {
                metadata.ids.push(normalize(id));
            } else if let Some(id) = provided.get("id").and_then(Value::as_str) {
                metadata.ids.push(normalize(id));
            }
        }
    }
    if let Some(depends) = loader.get("depends").and_then(Value::as_array) {
        for dependency in depends {
            if let Some(id) = dependency.as_str() {
                metadata.requires.push(normalize(id));
            } else if let Some(id) = dependency.get("id").and_then(Value::as_str) {
                metadata.requires.push(normalize(id));
            }
        }
    }
}

/// Line-based TOML reader for Forge/NeoForge `mods.toml`.
///
/// Only `[[mods]]` ids and `[[dependencies.<modid>]]` blocks with `mandatory`
/// (or NeoForge's `type = "required"`) are treated as required. Anything more
/// elaborate would need a full TOML parser and risk false positives.
fn collect_forge_toml(text: &str, loader: &'static str, metadata: &mut ModMetadata) {
    metadata.loader.get_or_insert(loader);
    let mut in_dependency = false;
    let mut mandatory = true;
    let mut pending = Vec::new();
    let flush = |pending: &mut Vec<String>, mandatory: bool, metadata: &mut ModMetadata| {
        if mandatory {
            metadata.requires.append(pending);
        } else {
            pending.clear();
        }
    };
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[[") {
            flush(&mut pending, mandatory, metadata);
            in_dependency = line.starts_with("[[dependencies");
            mandatory = true;
            continue;
        }
        if line.starts_with('[') {
            flush(&mut pending, mandatory, metadata);
            in_dependency = false;
            mandatory = true;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches(['"', '\'']);
        match key {
            "modId" if in_dependency => pending.push(normalize(value)),
            "modId" => {
                metadata.ids.push(normalize(value));
                metadata.primary_ids.push(normalize(value));
            }
            "mandatory" if in_dependency => mandatory = !value.eq_ignore_ascii_case("false"),
            "type" if in_dependency => {
                mandatory =
                    value.eq_ignore_ascii_case("required") || value.eq_ignore_ascii_case("both");
            }
            _ => {}
        }
    }
    flush(&mut pending, mandatory, metadata);
}

fn collect_mcmod_info(text: &str, metadata: &mut ModMetadata) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    metadata.loader.get_or_insert("forge");
    let entries = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(&value));
    for entry in entries {
        if let Some(id) = entry.get("modid").and_then(Value::as_str) {
            metadata.ids.push(normalize(id));
            metadata.primary_ids.push(normalize(id));
        }
        push_json_id(entry, "name", &mut metadata.ids);
        if let Some(required) = entry.get("requiredMods").and_then(Value::as_array) {
            for id in required.iter().filter_map(Value::as_str) {
                metadata.requires.push(normalize(id));
            }
        }
    }
}

fn push_json_id(value: &Value, key: &str, ids: &mut Vec<String>) {
    if let Some(id) = value.get(key).and_then(Value::as_str) {
        ids.push(normalize(id));
    }
}

/// Lowercase alphanumerics: Modrinth slugs, Fabric ids, and CurseForge names all
/// compare through this form.
pub fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Ids the loader or the Java runtime itself provides.
pub fn is_builtin_id(id: &str) -> bool {
    matches!(
        id,
        "fabricloader"
            | "minecraft"
            | "java"
            | "quiltloader"
            | "quiltbase"
            | "quiltedfabricloader"
            | "quiltedfabricapi"
            | "forge"
            | "neoforge"
            | "fml"
            | "javafml"
            | "lowcodefml"
            | "mixinextras"
            | "mixinextrasfabric"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        detect_loader, is_builtin_id, missing_required, normalize, read_jar, read_jar_cached,
        scan_dir,
    };
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_path(extension: &str) -> PathBuf {
        // Tests run in parallel and the system clock is not fine-grained
        // enough to promise distinct names on its own; a counter makes each
        // fixture file unique even when two tests start in the same tick.
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "kaelis-modmeta-{}-{}-{}.{extension}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn write_jar(entries: &[(&str, &[u8])]) -> PathBuf {
        let path = temp_path("jar");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    fn nested_jar(metadata: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            writer
                .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(metadata.as_bytes()).unwrap();
            writer.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn cached_metadata_reuses_unchanged_jars_and_notices_replacements() {
        let path = temp_path("jar");
        let write = |id: &str, padding: usize| {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            let metadata = format!(
                "{{\"id\":\"{id}\",\"description\":\"{}\"}}",
                "x".repeat(padding)
            );
            writer.write_all(metadata.as_bytes()).unwrap();
            writer.finish().unwrap();
        };
        write("alpha", 8);
        let first = read_jar_cached(&path);
        assert!(first.ids.contains(&"alpha".to_owned()));
        let cached = read_jar_cached(&path);
        assert_eq!(cached.ids, first.ids);
        write("beta", 4096);
        let replaced = read_jar_cached(&path);
        assert!(replaced.ids.contains(&"beta".to_owned()));
        assert!(!replaced.ids.contains(&"alpha".to_owned()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn normalizes_virtual_fabric_ids() {
        assert_eq!(normalize("fabric-command-api-v2"), "fabriccommandapiv2");
        assert_eq!(
            normalize("Fabric-Key-Binding-API-v1"),
            "fabrickeybindingapiv1"
        );
        assert!(is_builtin_id("fabricloader"));
        // Fabric API itself must be installable when a mod only names `fabricapi`.
        assert!(!is_builtin_id("fabricapi"));
        assert!(is_builtin_id("mixinextras"));
        assert!(!is_builtin_id("fabriccommandapiv2"));
    }

    #[test]
    fn reads_required_ids_from_fabric_metadata() {
        let path = write_jar(&[(
            "fabric.mod.json",
            br#"{"id":"ukus-armor-hud","depends":{"minecraft":"1.21.x","fabricloader":">=0.14","ukulib":">=1.0","fabric-command-api-v2":"*"}}"#,
        )]);
        let metadata = read_jar(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(metadata.loader, Some("fabric"));
        assert!(metadata.ids.contains(&"ukusarmorhud".to_owned()));
        assert_eq!(metadata.primary_ids, vec!["ukusarmorhud".to_owned()]);
        assert!(metadata.requires.contains(&"ukulib".to_owned()));
        assert!(metadata.requires.contains(&"fabriccommandapiv2".to_owned()));
    }

    #[test]
    fn indexes_nested_fabric_api_module_ids() {
        let nested =
            nested_jar(r#"{"id":"fabric-command-api-v2","provides":["fabric-command-api-v2"]}"#);
        let nested_meta = nested_jar(r#"{"id":"fabric-key-binding-api-v1","provides":[]}"#);
        let path = write_jar(&[
            (
                "fabric.mod.json",
                br#"{"id":"fabric-api","provides":["fabric"],"depends":{"minecraft":">=1.21"}}"#,
            ),
            ("META-INF/jars/fabric-command-api-v2.jar", nested.as_slice()),
            (
                "META-INF/jars/fabric-key-binding-api-v1.jar",
                nested_meta.as_slice(),
            ),
        ]);
        let metadata = read_jar(&path);
        let _ = std::fs::remove_file(&path);
        assert!(metadata.ids.contains(&"fabriccommandapiv2".to_owned()));
        assert!(metadata.ids.contains(&"fabrickeybindingapiv1".to_owned()));
        assert!(!metadata.requires.contains(&"fabriccommandapiv2".to_owned()));
        // Only the jar's own id belongs to the file; nested modules are shared
        // implementations and must not trigger duplicate-mod cleanup.
        assert_eq!(metadata.primary_ids, vec!["fabricapi".to_owned()]);
    }

    #[test]
    fn forge_toml_tracks_mandatory_dependencies_only() {
        let path = write_jar(&[(
            "META-INF/mods.toml",
            br#"modLoader="javafml"
loaderVersion="[47,)"
[[mods]]
modId="example_mod"
displayName="Example"
[[dependencies.example_mod]]
    modId="forge"
    mandatory=true
[[dependencies.example_mod]]
    modId="minecraft"
    mandatory=true
[[dependencies.example_mod]]
    modId="cloth_config"
    mandatory=true
[[dependencies.example_mod]]
    modId="optional_mod"
    mandatory=false
"#,
        )]);
        let metadata = read_jar(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(metadata.loader, Some("forge"));
        assert!(metadata.ids.contains(&"examplemod".to_owned()));
        assert_eq!(metadata.primary_ids, vec!["examplemod".to_owned()]);
        assert!(metadata.requires.contains(&"clothconfig".to_owned()));
        assert!(!metadata.requires.contains(&"optionalmod".to_owned()));
    }

    #[test]
    fn missing_required_ignores_builtins_and_satisfied_ids() {
        let dir = temp_path("dir");
        std::fs::create_dir_all(&dir).unwrap();
        let library = write_jar(&[(
            "fabric.mod.json",
            br#"{"id":"ukulib","provides":["ukulib"]}"#,
        )]);
        let consumer = write_jar(&[(
            "fabric.mod.json",
            br#"{"id":"ukus-armor-hud","depends":{"minecraft":"1.21","fabricloader":">=0.14","ukulib":">=1.0","cloth-config":"*"}}"#,
        )]);
        std::fs::copy(&library, dir.join("ukulib.jar")).unwrap();
        std::fs::copy(&consumer, dir.join("ukus-armor-hud.jar")).unwrap();
        std::fs::copy(&consumer, dir.join("ukus-armor-hud-2.jar.disabled")).unwrap();
        let missing = missing_required(&dir);
        assert_eq!(missing, vec!["clothconfig".to_owned()]);
        assert_eq!(detect_loader(&dir), Some("fabric"));
        assert_eq!(scan_dir(&dir).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&library);
        let _ = std::fs::remove_file(&consumer);
    }
}
