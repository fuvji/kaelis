//! Automatic mod environment repair.
//!
//! Scans an instance's `mods/` directory, maps every unsatisfied required mod id
//! to a Modrinth project, installs the resulting dependency graph, and clears
//! duplicate ids that would crash the loader. All work is bounded so a hostile
//! or cyclic metadata graph cannot pin a worker.

use crate::modmeta;
use crate::mods::{InstallReport, ModPlatformClient, ModProvider, ModSearchItem};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Modrinth project id for Fabric API.
pub const FABRIC_API_PROJECT: &str = "P7dR8mSH";
pub const MAX_REPAIR_ROUNDS: usize = 4;
pub const MAX_REPAIR_INSTALLS: usize = 24;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepairReport {
    pub installed: Vec<PathBuf>,
    pub resolved: Vec<String>,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
    /// Required ids that are still unsatisfied after the repair pass.
    pub unresolved: Vec<String>,
}

impl RepairReport {
    pub fn summary(&self) -> String {
        let count = self.installed.len();
        let noun = if count == 1 { "file" } else { "files" };
        let mut text = format!("Repaired environment · {count} {noun}");
        if !self.resolved.is_empty() {
            text.push_str(&format!(" · {} resolved", self.resolved.len()));
        }
        if !self.unresolved.is_empty() {
            text.push_str(&format!(" · {} unresolved", self.unresolved.len()));
        }
        if !self.warnings.is_empty() {
            text.push_str(&format!(" · {} warning(s)", self.warnings.len()));
        }
        text
    }

    fn merge_install(&mut self, report: InstallReport) {
        for path in report.installed {
            if !self.installed.contains(&path) {
                self.installed.push(path);
            }
        }
        self.notes.extend(report.notes);
        self.warnings.extend(report.warnings);
    }
}

/// Install every missing required dependency for `mods_dir`.
///
/// Fabric API is installed as a unit when any `fabric*` module is missing,
/// because its modules ship as nested jars inside that one project. Remaining
/// ids are resolved through Modrinth by id, slug, or title. The loop re-scans
/// after each round so dependencies of newly installed mods are covered too.
pub async fn repair_environment(
    platforms: &ModPlatformClient,
    game_version: &str,
    loader: &str,
    mods_dir: &Path,
) -> RepairReport {
    let mut report = RepairReport::default();
    if let Err(error) = std::fs::create_dir_all(mods_dir) {
        report
            .warnings
            .push(format!("could not create the mods folder: {error}"));
        return report;
    }
    if loader.eq_ignore_ascii_case("vanilla") {
        report.warnings.push(
            "this instance has no mod loader; install Fabric, Quilt, Forge or NeoForge first"
                .to_owned(),
        );
        return report;
    }
    let fabric_family =
        loader.eq_ignore_ascii_case("fabric") || loader.eq_ignore_ascii_case("quilt");
    // A modded instance is unusable without its loader, so provision Fabric
    // before resolving any jars. The marker makes repeat calls network-free.
    if loader.eq_ignore_ascii_case("fabric")
        && let Some(root) = mods_dir.parent()
    {
        match crate::loader::ensure_installed(platforms.http(), root, game_version).await {
            Ok(install) => report.notes.push(format!(
                "Fabric loader {} is ready ({} libraries).",
                install.loader_version, install.libraries
            )),
            Err(error) => report
                .warnings
                .push(format!("Fabric loader could not be provisioned: {error}")),
        }
    }
    if let Some(detected) = modmeta::detect_loader(mods_dir)
        && !detected.eq_ignore_ascii_case(loader)
        && !(fabric_family && detected.eq_ignore_ascii_case("quilt"))
    {
        report.warnings.push(format!(
            "installed jars look like {detected}, but the instance targets {loader}; dependency resolution will follow {loader}"
        ));
    }
    let mut attempted = HashSet::new();

    for _round in 0..MAX_REPAIR_ROUNDS {
        let missing = modmeta::missing_required(mods_dir);
        let pending = missing
            .into_iter()
            .filter(|id| !attempted.contains(id))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        let mut progressed = false;

        if fabric_family
            && pending.iter().any(|id| id.starts_with("fabric"))
            && attempted.insert("fabric-api".to_owned())
        {
            report.notes.push(
                "Missing Fabric API modules detected; installing Fabric API to provide them."
                    .to_owned(),
            );
            let item = ModSearchItem {
                provider: ModProvider::Modrinth,
                id: FABRIC_API_PROJECT.to_owned(),
                slug: "fabric-api".to_owned(),
                title: "Fabric API".to_owned(),
                description: String::new(),
                downloads: 0,
                icon_url: None,
                ..ModSearchItem::default()
            };
            match platforms
                .install_with_dependencies(&item, game_version, loader, mods_dir, true)
                .await
            {
                Ok(install) => {
                    if !install.installed.is_empty() {
                        progressed = true;
                    }
                    report.merge_install(install);
                }
                Err(error) => report
                    .warnings
                    .push(format!("Fabric API could not be installed: {error}")),
            }
        }

        let mut installs = 0;
        for id in pending {
            if id.starts_with("fabric") {
                if fabric_family {
                    // Fabric API supplies these; a separate project install
                    // would create a duplicate or install an unrelated mod.
                    continue;
                }
                report.warnings.push(format!(
                    "`{id}` is a Fabric module and cannot be installed on a {loader} instance"
                ));
                continue;
            }
            if installs >= MAX_REPAIR_INSTALLS || !attempted.insert(id.clone()) {
                continue;
            }
            let Some(item) = platforms.resolve_project(&id, game_version, loader).await else {
                report.unresolved.push(id);
                continue;
            };
            installs += 1;
            report.notes.push(format!(
                "Resolved `{id}` to {} ({})",
                item.title, item.provider
            ));
            match platforms
                .install_with_dependencies(&item, game_version, loader, mods_dir, true)
                .await
            {
                Ok(install) => {
                    if !install.installed.is_empty() {
                        progressed = true;
                        report.resolved.push(id);
                    }
                    report.merge_install(install);
                }
                Err(error) => report
                    .warnings
                    .push(format!("`{id}` could not be installed: {error}")),
            }
        }

        if !progressed {
            break;
        }
    }

    // The final scan is the authority: an install can silently bring its own
    // unsatisfied ids, and a failed download must show up as unresolved even
    // when resolution succeeded earlier in the pass.
    report.unresolved = modmeta::missing_required(mods_dir);
    report
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DuplicateRepair {
    pub disabled: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

impl DuplicateRepair {
    pub fn summary(&self) -> String {
        let count = self.disabled.len();
        let noun = if count == 1 { "jar" } else { "jars" };
        format!("Disabled {count} duplicate {noun}")
    }
}

/// Rename older jars that provide an id another enabled jar also provides.
///
/// A jar is only disabled when every id it provides remains satisfied by other
/// enabled jars, so unique modules are never lost. The newest jar (by mtime,
/// then size) wins each conflicting id.
pub fn disable_duplicate_mods(mods_dir: &Path) -> std::io::Result<DuplicateRepair> {
    let mut outcome = DuplicateRepair::default();
    for _pass in 0..16 {
        let mods = modmeta::scan_dir(mods_dir);
        if mods.is_empty() {
            break;
        }
        let mut providers: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut owners: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, local) in mods.iter().enumerate() {
            if local.disabled {
                continue;
            }
            for id in &local.metadata.ids {
                providers.entry(id.clone()).or_default().push(index);
            }
            // Only a top-level id makes two jars the same mod. Jar-in-jar
            // modules (Sodium and ukulib bundle parts of Fabric API) are
            // shared implementions, not competing copies.
            for id in &local.metadata.primary_ids {
                owners.entry(id.clone()).or_default().push(index);
            }
        }
        let mut planned: HashSet<usize> = HashSet::new();
        for (id, indices) in &owners {
            if indices.len() < 2 {
                continue;
            }
            let keeper = indices
                .iter()
                .copied()
                .filter(|index| !planned.contains(index))
                .max_by_key(|index| freshness(&mods[*index].path));
            let Some(keeper) = keeper else {
                continue;
            };
            for &index in indices {
                if index == keeper || planned.contains(&index) {
                    continue;
                }
                let candidate = &mods[index];
                let safe = candidate.metadata.ids.iter().all(|other| {
                    providers.get(other).is_some_and(|owners| {
                        owners
                            .iter()
                            .any(|owner| *owner != index && !planned.contains(owner))
                    })
                });
                if safe {
                    planned.insert(index);
                } else {
                    outcome.warnings.push(format!(
                        "kept {} because it uniquely provides `{}`",
                        candidate.name, id
                    ));
                }
            }
        }
        if planned.is_empty() {
            break;
        }
        for index in planned {
            let candidate = &mods[index];
            let target = unique_disabled_path(&candidate.path);
            match std::fs::rename(&candidate.path, &target) {
                Ok(()) => {
                    outcome
                        .notes
                        .push(format!("Disabled duplicate {}", candidate.name));
                    outcome.disabled.push(target);
                }
                Err(error) => outcome
                    .warnings
                    .push(format!("could not disable {}: {error}", candidate.name)),
            }
        }
    }
    Ok(outcome)
}

fn freshness(path: &Path) -> (u128, u64) {
    let metadata = std::fs::metadata(path).ok();
    let modified = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let size = metadata.map(|metadata| metadata.len()).unwrap_or(0);
    (modified, size)
}

fn unique_disabled_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let base = format!("{name}.disabled");
    let mut candidate = path.with_file_name(&base);
    let mut counter = 1_u32;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{base}.{counter}"));
        counter += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::{RepairReport, disable_duplicate_mods};
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kaelis-repair-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_mod(dir: &Path, name: &str, metadata: &str, padding: usize) {
        write_mod_with_nested(dir, name, metadata, &[], padding);
    }

    fn nested_module(metadata: &str) -> Vec<u8> {
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

    fn write_mod_with_nested(
        dir: &Path,
        name: &str,
        metadata: &str,
        nested: &[(&str, &[u8])],
        padding: usize,
    ) {
        let file = std::fs::File::create(dir.join(name)).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(metadata.as_bytes()).unwrap();
        for (path, bytes) in nested {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer
            .start_file("padding.bin", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&vec![0_u8; padding]).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn disables_the_older_duplicate_only() {
        let dir = temp_dir();
        write_mod(&dir, "sodium-old.jar", r#"{"id":"sodium"}"#, 1);
        write_mod(&dir, "sodium-new.jar", r#"{"id":"sodium"}"#, 4096);
        let outcome = disable_duplicate_mods(&dir).unwrap();
        assert_eq!(outcome.disabled.len(), 1);
        assert!(!dir.join("sodium-old.jar").exists());
        assert!(dir.join("sodium-new.jar").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_a_duplicate_that_uniquely_provides_another_id() {
        let dir = temp_dir();
        write_mod(
            &dir,
            "sodium-old.jar",
            r#"{"id":"sodium","provides":["legacycompat"]}"#,
            1,
        );
        write_mod(&dir, "sodium-new.jar", r#"{"id":"sodium"}"#, 4096);
        let outcome = disable_duplicate_mods(&dir).unwrap();
        assert!(outcome.disabled.is_empty());
        assert!(!outcome.warnings.is_empty());
        assert!(dir.join("sodium-old.jar").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_fabric_modules_are_not_treated_as_duplicate_mods() {
        let dir = temp_dir();
        // Sodium and ukulib ship older copies of Fabric API modules inside
        // their jars. Fabric Loader resolves those by id; the launcher must not
        // disable either top-level mod because the module ids overlap.
        let module = nested_module(r#"{"id":"fabric-api-base","name":"Fabric API Base"}"#);
        write_mod_with_nested(
            &dir,
            "sodium-fabric.jar",
            r#"{"id":"sodium","provides":["indium"]}"#,
            &[("META-INF/jars/fabric-api-base.jar", module.as_slice())],
            2048,
        );
        write_mod_with_nested(
            &dir,
            "fabric-api.jar",
            r#"{"id":"fabric-api","provides":["fabric"]}"#,
            &[("META-INF/jars/fabric-api-base.jar", module.as_slice())],
            4096,
        );
        let outcome = disable_duplicate_mods(&dir).unwrap();
        assert!(outcome.disabled.is_empty());
        assert!(dir.join("sodium-fabric.jar").exists());
        assert!(dir.join("fabric-api.jar").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_summary_counts_resolved_and_unresolved() {
        let mut report = RepairReport::default();
        report.installed.push("a.jar".into());
        report.resolved.push("ukulib".to_owned());
        assert_eq!(
            report.summary(),
            "Repaired environment · 1 file · 1 resolved"
        );
        report.unresolved.push("clothconfig".to_owned());
        report.warnings.push("offline".to_owned());
        assert!(report.summary().ends_with("1 unresolved · 1 warning(s)"));
    }
}
