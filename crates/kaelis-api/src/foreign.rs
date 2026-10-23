//! Read instance archives written by other launchers.
//!
//! Kaelis can already move its own `.kaelis.zip` between machines. A player
//! arriving from Prism, MultiMC, CurseForge, or a hand-made pack should not be
//! told to start over, so this module probes an archive by its contents and
//! understands the layouts that actually exist in the wild:
//!
//! - Prism / MultiMC: `instance.cfg` + `mmc-pack.json` beside `.minecraft/`.
//! - CurseForge: `manifest.json` + `overrides/`.
//! - Plain packs: a zip whose entries are `mods/`, `config/`, `options.txt`, …
//!
//! Modrinth packs are detected here but installed by `mrpack`, which knows how
//! to resolve their download URLs. Kaelis archives are delegated to `portable`.
//!
//! Everything is read defensively: absolute paths, `..`, symlinks, oversized
//! entries, and archives that expand past the limit are refused before a byte
//! is written.
use kaelis_core::instance::Instance;
use serde_json::Value;
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, Error)]
pub enum ForeignError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("this archive is not a Minecraft instance")]
    Unrecognised,
    #[error("the archive does not declare a Minecraft version")]
    MissingVersion,
    #[error("refusing unsafe archive path: {0}")]
    UnsafePath(String),
    #[error("archive entry is too large: {0}")]
    EntryTooLarge(String),
    #[error("archive expands past its size limit")]
    ArchiveTooLarge,
    #[error("archive has more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    #[error("archive layout is not supported: {0}")]
    Unsupported(String),
}

/// Which launcher wrote the archive, and what it says about its game profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveProbe {
    pub kind: ForeignKind,
    /// Display name from the pack metadata, if it has one.
    pub name: String,
    pub game_version: String,
    /// `vanilla`, `fabric`, `quilt`, `forge`, or `neoforge`.
    pub loader: String,
    /// Pack files that must be fetched from a provider (CurseForge files).
    pub remote_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignKind {
    Kaelis,
    Modrinth,
    Prism,
    CurseForge,
    /// A zip of game files with no launcher manifest.
    Plain,
}

impl ForeignKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Kaelis => "Kaelis",
            Self::Modrinth => "Modrinth",
            Self::Prism => "Prism / MultiMC",
            Self::CurseForge => "CurseForge",
            Self::Plain => "loose game files",
        }
    }
}

/// What an import produced, including anything the user still has to fetch.
#[derive(Debug, Clone)]
pub struct ForeignImport {
    pub instance: Instance,
    pub files: u64,
    pub bytes: u64,
    pub remote_files: usize,
}

impl ForeignImport {
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} · {} file{} · {}",
            self.instance.name,
            self.files,
            if self.files == 1 { "" } else { "s" },
            kaelis_core::human_bytes(self.bytes)
        );
        if self.remote_files > 0 {
            text.push_str(&format!(
                " · {} pack file{} need downloading from CurseForge",
                self.remote_files,
                if self.remote_files == 1 { "" } else { "s" }
            ));
        }
        text
    }
}

/// Identify an archive without extracting it.
pub fn probe_archive(path: &Path) -> Result<ArchiveProbe, ForeignError> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    if zip.len() > MAX_ENTRIES {
        return Err(ForeignError::TooManyEntries);
    }
    if has_entry(&mut zip, "kaelis-instance.json") {
        let name = read_json_entry(&mut zip, "kaelis-instance.json")?;
        return Ok(ArchiveProbe {
            kind: ForeignKind::Kaelis,
            name: name
                .get("instance")
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            game_version: name
                .get("instance")
                .and_then(|value| value.get("mc_version"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            loader: name
                .get("instance")
                .and_then(|value| value.get("loader"))
                .and_then(Value::as_str)
                .unwrap_or("vanilla")
                .to_owned(),
            remote_files: 0,
        });
    }
    if has_entry(&mut zip, "modrinth.index.json") {
        let index = read_json_entry(&mut zip, "modrinth.index.json")?;
        let dependencies = index.get("dependencies").cloned().unwrap_or_default();
        let game_version = dependencies
            .get("minecraft")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let loader = if dependencies.get("fabric-loader").is_some() {
            "fabric"
        } else if dependencies.get("quilt-loader").is_some() {
            "quilt"
        } else if dependencies.get("neoforge").is_some() {
            "neoforge"
        } else if dependencies.get("forge").is_some() {
            "forge"
        } else {
            "vanilla"
        };
        return Ok(ArchiveProbe {
            kind: ForeignKind::Modrinth,
            name: index
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    index
                        .get("versionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default(),
            game_version,
            loader: loader.to_owned(),
            remote_files: index
                .get("files")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        });
    }
    if has_entry(&mut zip, "mmc-pack.json") || has_entry(&mut zip, "instance.cfg") {
        return probe_prism(&mut zip);
    }
    if has_entry(&mut zip, "manifest.json") {
        return probe_curseforge(&mut zip);
    }
    // Some Prism exports nest the whole instance one folder down.
    if let Some(prefix) = single_root_prefix(&mut zip)? {
        if has_entry(&mut zip, &format!("{prefix}mmc-pack.json"))
            || has_entry(&mut zip, &format!("{prefix}instance.cfg"))
        {
            return probe_prism(&mut zip);
        }
        if has_entry(&mut zip, &format!("{prefix}manifest.json")) {
            return probe_curseforge(&mut zip);
        }
        if has_entry(&mut zip, &format!("{prefix}modrinth.index.json")) {
            return Err(ForeignError::Unsupported("nested Modrinth pack".to_owned()));
        }
    }
    if looks_like_game_files(&mut zip)? {
        return Ok(ArchiveProbe {
            kind: ForeignKind::Plain,
            name: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            game_version: String::new(),
            loader: String::new(),
            remote_files: 0,
        });
    }
    Err(ForeignError::Unrecognised)
}

/// Extract a probed archive into a fresh instance record.
///
/// `fallback_version` is only used by loose game-file packs, which do not
/// name a Minecraft version; the caller supplies the version the player picked
/// in the create panel. Prism/CurseForge archives carry their own version.
pub fn import_foreign_archive(
    path: &Path,
    probe: &ArchiveProbe,
    instances_dir: &Path,
    fallback_version: &str,
) -> Result<ForeignImport, ForeignError> {
    if matches!(probe.kind, ForeignKind::Kaelis | ForeignKind::Modrinth) {
        return Err(ForeignError::Unsupported(
            "this archive has a dedicated importer".to_owned(),
        ));
    }
    let game_version = if probe.game_version.trim().is_empty() {
        fallback_version.trim()
    } else {
        probe.game_version.trim()
    };
    if game_version.is_empty() {
        return Err(ForeignError::MissingVersion);
    }
    let loader = if probe.loader.trim().is_empty() {
        detect_loader(path)
    } else {
        probe.loader.clone()
    };
    let name = if probe.name.trim().is_empty() {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Imported instance".to_owned())
    } else {
        probe.name.trim().to_owned()
    };
    let (id, root) = unique_slot(&name, instances_dir)?;
    std::fs::create_dir_all(&root)?;
    let result = extract_foreign(path, probe.kind, &root);
    let (files, bytes) = match result {
        Ok(counts) => counts,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
    };
    let mut instance = Instance::new(&id, &name, game_version, &loader, root);
    let icon = instance.root.join("icon.png");
    if icon.is_file() {
        instance.icon = Some(icon);
    }
    if let Err(error) = instance.save_in(instances_dir) {
        let _ = std::fs::remove_dir_all(&instance.root);
        return Err(ForeignError::Io(std::io::Error::other(error.to_string())));
    }
    Ok(ForeignImport {
        instance,
        files,
        bytes,
        remote_files: probe.remote_files,
    })
}

fn probe_prism<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Result<ArchiveProbe, ForeignError> {
    let prefix = find_prefix(zip, "mmc-pack.json").unwrap_or_default();
    let pack = read_json_entry(zip, &format!("{prefix}mmc-pack.json")).ok();
    let mut game_version = String::new();
    let mut loader = "vanilla".to_owned();
    if let Some(components) = pack
        .as_ref()
        .and_then(|value| value.get("components"))
        .and_then(Value::as_array)
    {
        for component in components {
            let uid = component.get("uid").and_then(Value::as_str).unwrap_or("");
            let version = component
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("");
            match uid {
                "net.minecraft" => game_version = version.to_owned(),
                "net.fabricmc.fabric-loader" | "org.quiltmc.quilt-loader" => {
                    loader = if uid.contains("quilt") {
                        "quilt"
                    } else {
                        "fabric"
                    }
                    .to_owned()
                }
                "net.minecraftforge" => loader = "forge".to_owned(),
                "net.neoforged" => loader = "neoforge".to_owned(),
                _ => {}
            }
        }
    }
    let name = read_entry_string(zip, &format!("{prefix}instance.cfg"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("name=")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            })
        })
        .unwrap_or_default();
    Ok(ArchiveProbe {
        kind: ForeignKind::Prism,
        name,
        game_version,
        loader,
        remote_files: 0,
    })
}

fn probe_curseforge<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<ArchiveProbe, ForeignError> {
    let prefix = find_prefix(zip, "manifest.json").unwrap_or_default();
    let manifest = read_json_entry(zip, &format!("{prefix}manifest.json"))?;
    let minecraft = manifest.get("minecraft").cloned().unwrap_or_default();
    let game_version = minecraft
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let loader = minecraft
        .get("modLoaders")
        .and_then(Value::as_array)
        .and_then(|loaders| {
            loaders
                .iter()
                .find(|loader| loader.get("primary").and_then(Value::as_bool) == Some(true))
                .or_else(|| loaders.first())
        })
        .and_then(|loader| loader.get("id"))
        .and_then(Value::as_str)
        .map(|id| {
            let id = id.to_ascii_lowercase();
            if id.starts_with("fabric") {
                "fabric"
            } else if id.starts_with("quilt") {
                "quilt"
            } else if id.starts_with("neoforge") {
                "neoforge"
            } else if id.starts_with("forge") {
                "forge"
            } else {
                "vanilla"
            }
        })
        .unwrap_or("vanilla")
        .to_owned();
    let remote_files = manifest
        .get("files")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    Ok(ArchiveProbe {
        kind: ForeignKind::CurseForge,
        name: manifest
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        game_version,
        loader,
        remote_files,
    })
}

/// Copy the game files out of an archive, stripping each launcher's wrapper.
fn extract_foreign(
    path: &Path,
    kind: ForeignKind,
    root: &Path,
) -> Result<(u64, u64), ForeignError> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    if zip.len() > MAX_ENTRIES {
        return Err(ForeignError::TooManyEntries);
    }
    let single = single_root_prefix(&mut zip)?;
    let mut files = 0_u64;
    let mut total = 0_u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let raw = entry.name().to_owned();
        if raw.ends_with('/') {
            continue;
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(ForeignError::UnsafePath(raw));
        }
        let Some(relative) = map_entry(&raw, kind, single.as_deref()) else {
            continue;
        };
        let declared = entry.size();
        if declared > MAX_ENTRY_BYTES {
            return Err(ForeignError::EntryTooLarge(raw));
        }
        total = total.saturating_add(declared);
        if total > MAX_TOTAL_BYTES {
            return Err(ForeignError::ArchiveTooLarge);
        }
        let destination = safe_join(root, &relative)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = std::fs::File::create(&destination)?;
        let written = kaelis_core::stream::copy_stream(
            &mut (&mut entry).take(MAX_ENTRY_BYTES + 1),
            &mut output,
        )?;
        if written > MAX_ENTRY_BYTES {
            let _ = std::fs::remove_file(&destination);
            return Err(ForeignError::EntryTooLarge(raw));
        }
        files += 1;
    }
    if files == 0 {
        return Err(ForeignError::Unsupported(
            "the archive contains no game files".to_owned(),
        ));
    }
    Ok((files, total))
}

/// Which archive path becomes which instance path. `None` drops launcher
/// metadata that has no meaning outside the original launcher.
fn map_entry(raw: &str, kind: ForeignKind, single: Option<&str>) -> Option<PathBuf> {
    let mut name = raw.replace('\\', "/");
    if let Some(prefix) = single {
        name = name.strip_prefix(prefix)?.to_owned();
    }
    match kind {
        ForeignKind::Prism => {
            if name == "instance.cfg" || name == "mmc-pack.json" || name == "mmc-pack.json.bak" {
                return None;
            }
            let name = name
                .strip_prefix(".minecraft/")
                .or_else(|| name.strip_prefix("minecraft/"))
                .unwrap_or(&name);
            if name.is_empty() {
                return None;
            }
            Some(PathBuf::from(name))
        }
        ForeignKind::CurseForge => {
            if name == "manifest.json" || name == "modlist.html" {
                return None;
            }
            let name = name.strip_prefix("overrides/").unwrap_or(&name);
            if name.is_empty() || name == "manifest.json" {
                return None;
            }
            Some(PathBuf::from(name))
        }
        ForeignKind::Plain => {
            let lowered = name.to_ascii_lowercase();
            if lowered == ".ds_store" || lowered == "thumbs.db" || lowered == "desktop.ini" {
                return None;
            }
            Some(PathBuf::from(name))
        }
        ForeignKind::Kaelis | ForeignKind::Modrinth => None,
    }
}

fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf, ForeignError> {
    let mut destination = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => destination.push(segment),
            Component::CurDir => {}
            _ => {
                return Err(ForeignError::UnsafePath(relative.display().to_string()));
            }
        }
    }
    if !destination.starts_with(root) {
        return Err(ForeignError::UnsafePath(relative.display().to_string()));
    }
    Ok(destination)
}

fn has_entry<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> bool {
    let needle = name.replace('\\', "/");
    (0..zip.len()).any(|index| {
        zip.by_index_raw(index)
            .map(|entry| {
                let raw = entry.name().replace('\\', "/");
                raw == needle || raw.ends_with(&format!("/{needle}"))
            })
            .unwrap_or(false)
    })
}

fn find_prefix<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).ok()?;
        let raw = entry.name().replace('\\', "/");
        if raw == name {
            return Some(String::new());
        }
        if let Some(prefix) = raw.strip_suffix(name)
            && prefix.ends_with('/')
        {
            return Some(prefix.to_owned());
        }
    }
    None
}

/// The one wrapper folder every entry shares (`pack-name/`), if any.
fn single_root_prefix<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<Option<String>, ForeignError> {
    let mut prefix: Option<String> = None;
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index)?;
        let raw = entry.name().replace('\\', "/");
        let trimmed = raw.trim_start_matches("./");
        let Some((first, _)) = trimmed.split_once('/') else {
            return Ok(None);
        };
        if first.is_empty() {
            return Ok(None);
        }
        match &prefix {
            Some(existing) if existing != first => return Ok(None),
            Some(_) => {}
            None => prefix = Some(format!("{first}/")),
        }
    }
    Ok(prefix)
}

fn looks_like_game_files<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<bool, ForeignError> {
    const MARKERS: [&str; 6] = [
        "mods/",
        "config/",
        "options.txt",
        "resourcepacks/",
        "shaderpacks/",
        "saves/",
    ];
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index)?;
        let raw = entry.name().replace('\\', "/");
        let relative = raw.trim_start_matches("./");
        if MARKERS
            .iter()
            .any(|marker| relative.starts_with(marker) || relative.contains(&format!("/{marker}")))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_json_entry<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Value, ForeignError> {
    let text = read_entry_string(zip, name)?;
    Ok(serde_json::from_str(&text)?)
}

fn read_entry_string<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<String, ForeignError> {
    let mut entry = zip.by_name(name)?;
    if entry.size() > MAX_MANIFEST_BYTES {
        return Err(ForeignError::EntryTooLarge(name.to_owned()));
    }
    let mut text = String::new();
    entry
        .by_ref()
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)?;
    Ok(text)
}

fn choose_id(name: &str) -> String {
    let mut id: String = name
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
    let id = id.trim_matches('-');
    if id.is_empty() {
        "imported-instance".to_owned()
    } else {
        id.chars().take(48).collect()
    }
}

fn unique_slot(name: &str, instances_dir: &Path) -> Result<(String, PathBuf), ForeignError> {
    let base = choose_id(name);
    for attempt in 0..1000_u32 {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{}", attempt + 1)
        };
        let record = instances_dir.join(format!("{candidate}.json"));
        let folder = instances_dir.join(&candidate);
        if !record.exists() && !folder.exists() {
            return Ok((candidate, folder));
        }
    }
    Err(ForeignError::Unsupported(
        "no free instance name is available".to_owned(),
    ))
}

/// Best-effort loader guess for a loose pack: the jars a loader ships with are
/// a stronger signal than the folder name.
fn detect_loader(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return "vanilla".to_owned();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return "vanilla".to_owned();
    };
    let mut loader = "vanilla";
    for index in 0..zip.len() {
        let Ok(entry) = zip.by_index_raw(index) else {
            continue;
        };
        let name = entry.name().replace('\\', "/").to_ascii_lowercase();
        if !name.ends_with(".jar") {
            continue;
        }
        if name.contains("fabric-api") || name.contains("fabric_loader") {
            return "fabric".to_owned();
        }
        if name.contains("quilt") {
            loader = "quilt";
        } else if name.contains("neoforge") {
            return "neoforge".to_owned();
        } else if name.contains("forge") {
            loader = "forge";
        }
    }
    loader.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn build(label: &str, entries: &[(&str, &str)]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kaelis-foreign-{label}-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, body) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    #[test]
    fn recognises_a_prism_instance_and_its_pack() {
        let archive = build(
            "prism",
            &[
                (
                    "mmc-pack.json",
                    r#"{"formatVersion":1,"components":[{"uid":"net.minecraft","version":"1.21.1"},{"uid":"net.fabricmc.fabric-loader","version":"0.16.5"}]}"#,
                ),
                ("instance.cfg", "[General]\nname=Kitchen Sink\n"),
                (".minecraft/mods/sodium.jar", "jar"),
                (".minecraft/options.txt", "fov=90"),
            ],
        );
        let probe = probe_archive(&archive).unwrap();
        assert_eq!(probe.kind, ForeignKind::Prism);
        assert_eq!(probe.name, "Kitchen Sink");
        assert_eq!(probe.game_version, "1.21.1");
        assert_eq!(probe.loader, "fabric");

        let instances = archive
            .parent()
            .unwrap()
            .join(format!("out-{}", std::process::id()));
        std::fs::create_dir_all(&instances).unwrap();
        let report = import_foreign_archive(&archive, &probe, &instances, "").unwrap();
        assert_eq!(report.instance.mc_version, "1.21.1");
        assert!(report.instance.root.join("mods/sodium.jar").is_file());
        assert!(report.instance.root.join("options.txt").is_file());
        assert!(!report.instance.root.join("instance.cfg").exists());
        let _ = std::fs::remove_dir_all(&instances);
        let _ = std::fs::remove_file(&archive);
    }

    #[test]
    fn recognises_a_curseforge_manifest_and_uses_overrides() {
        let archive = build(
            "cf",
            &[
                (
                    "manifest.json",
                    r#"{"minecraft":{"version":"1.20.1","modLoaders":[{"id":"forge-47.2.0","primary":true}]},"name":"Create Pack","files":[{"projectID":1,"fileID":2,"required":true}]}"#,
                ),
                ("overrides/config/create.json", "{}"),
                ("overrides/mods/create.jar", "jar"),
            ],
        );
        let probe = probe_archive(&archive).unwrap();
        assert_eq!(probe.kind, ForeignKind::CurseForge);
        assert_eq!(probe.loader, "forge");
        assert_eq!(probe.remote_files, 1);
        let instances = archive
            .parent()
            .unwrap()
            .join(format!("out-cf-{}", std::process::id()));
        std::fs::create_dir_all(&instances).unwrap();
        let report = import_foreign_archive(&archive, &probe, &instances, "").unwrap();
        assert!(report.instance.root.join("config/create.json").is_file());
        assert!(!report.instance.root.join("manifest.json").exists());
        let _ = std::fs::remove_dir_all(&instances);
        let _ = std::fs::remove_file(&archive);
    }

    #[test]
    fn wraps_a_loose_pack_in_a_new_instance_and_rejects_path_traversal() {
        let archive = build(
            "plain",
            &[("mods/sodium.jar", "jar"), ("options.txt", "fov=90")],
        );
        let probe = probe_archive(&archive).unwrap();
        assert_eq!(probe.kind, ForeignKind::Plain);
        assert_eq!(
            import_foreign_archive(&archive, &probe, archive.parent().unwrap(), "")
                .unwrap_err()
                .to_string(),
            ForeignError::MissingVersion.to_string()
        );

        let hostile = build(
            "hostile",
            &[("mods/ok.jar", "jar"), ("../escape.txt", "no")],
        );
        let probe = ArchiveProbe {
            kind: ForeignKind::Plain,
            name: "Hostile".to_owned(),
            game_version: "1.21.1".to_owned(),
            loader: "vanilla".to_owned(),
            remote_files: 0,
        };
        let instances =
            std::env::temp_dir().join(format!("kaelis-foreign-out-{}", std::process::id()));
        std::fs::create_dir_all(&instances).unwrap();
        let error = import_foreign_archive(&hostile, &probe, &instances, "").unwrap_err();
        assert!(matches!(error, ForeignError::UnsafePath(_)));
        let _ = std::fs::remove_dir_all(&instances);
        let _ = std::fs::remove_file(&archive);
        let _ = std::fs::remove_file(&hostile);
    }
}
