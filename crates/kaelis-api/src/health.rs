//! Pre-launch readiness and automatic repair.
//!
//! A launch used to fail wherever a piece was missing: a formatted machine had
//! no Java, a half-finished download had no client jar, and a deleted library
//! only surfaced as a stack trace inside the game's log. [`check`] answers the
//! question the Play button actually asks — can this instance start right now?
//! — and [`repair`] fills every gap it can reach from Mojang's own metadata:
//! the version-matched Java runtime, the missing client files, and a mod
//! loader (Fabric and Quilt from their meta, Forge and NeoForge through their
//! own headless installer).
//!
//! [`check`] is deliberately network-free and cheap (a handful of metadata
//! reads, plus one filesystem pass over the game assets only when an install
//! has no completion marker). A healthy instance therefore pays nothing, and
//! `repair` is only ever reached when something is genuinely absent.

use crate::runtime;
use crate::vanilla;
use crate::versions;
use kaelis_core::instance::Instance;
use std::path::PathBuf;
use std::sync::Arc;

/// What a launch needs that is missing right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Readiness {
    /// A JVM this instance can start: its own runtime, a configured path, the
    /// `PATH`, or an installed JDK.
    pub java: bool,
    /// Game files (metadata, client jar, libraries, assets) that are absent.
    pub missing_files: usize,
    /// A mod loader that is configured but not provisioned yet.
    pub loader: Option<String>,
    /// Whether [`repair`] can install that loader on its own. Kaelis manages
    /// Fabric, Quilt, Forge and NeoForge; an unrecognised loader is reported
    /// instead of guessed at.
    pub loader_repairable: bool,
}

impl Readiness {
    /// Nothing to repair: the game can start on its own.
    pub fn ready(&self) -> bool {
        self.java && self.missing_files == 0 && self.loader.is_none()
    }

    /// One short line per gap, in the order a repair closes them.
    pub fn issues(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if !self.java {
            issues.push("no Java runtime for this instance".to_owned());
        }
        if self.missing_files > 0 {
            issues.push(format!("{} missing game file(s)", self.missing_files));
        }
        if let Some(loader) = &self.loader {
            issues.push(if self.loader_repairable {
                format!("{loader} loader is not installed")
            } else {
                format!("{loader} loader is not installed and needs its own installer")
            });
        }
        issues
    }

    /// Status-bar text for a check that found work to do.
    pub fn summary(&self) -> String {
        let issues = self.issues();
        if issues.is_empty() {
            return "Instance is ready.".to_owned();
        }
        format!("Repairing {}…", issues.join(", "))
    }
}

/// Check an instance without touching the network.
pub fn check(instance: &Instance) -> Readiness {
    let java = runtime::resolve_java(instance).is_some();
    let missing_files = match vanilla::missing_install_files(&instance.root, &instance.mc_version) {
        Ok(missing) => missing.len(),
        // Unreadable metadata counts as a gap; the repair path re-fetches it.
        Err(_) => 1,
    };
    let loader_name = instance.loader.trim().to_ascii_lowercase();
    let unprovisioned = !loader_name.is_empty()
        && loader_name != "vanilla"
        && crate::loader::load_for(&instance.root, &instance.mc_version, &loader_name).is_none();
    let (loader, loader_repairable) = if unprovisioned {
        (
            Some(loader_name.clone()),
            matches!(
                loader_name.as_str(),
                "fabric" | "quilt" | "forge" | "neoforge"
            ),
        )
    } else {
        (None, false)
    };
    Readiness {
        java,
        missing_files,
        loader,
        loader_repairable,
    }
}

/// Display name for a loader id, used in progress and report lines.
fn loader_label(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        "forge" => "Forge",
        "neoforge" => "NeoForge",
        _ => "Loader",
    }
}

/// Progress from a repair pass, for a status bar and a progress bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairProgress {
    /// What is happening now.
    Message(String),
    /// A download phase: how many files are done out of how many are needed.
    Download { completed: usize, total: usize },
}

/// What [`repair`] actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// Runtime installed by this pass, if a JVM had to be fetched.
    pub java: Option<PathBuf>,
    /// Missing game files that were downloaded.
    pub game_files: usize,
    /// Loader provisioned by this pass, if any.
    pub loader: Option<String>,
    pub notes: Vec<String>,
}

impl RepairReport {
    pub fn summary(&self) -> String {
        if self.java.is_none() && self.game_files == 0 && self.loader.is_none() {
            return "Instance is already complete.".to_owned();
        }
        let mut parts = Vec::new();
        if let Some(java) = &self.java {
            parts.push(format!("Java at {}", java.display()));
        }
        if self.game_files > 0 {
            parts.push(format!("{} game file(s) restored", self.game_files));
        }
        if let Some(loader) = &self.loader {
            parts.push(format!("{loader} loader installed"));
        }
        format!("Repaired · {}", parts.join(" · "))
    }
}

/// Fill every gap [`check`] reported, using Mojang's metadata and the loader
/// endpoints as the only sources.
///
/// Stages run in launch order: the client files, then the JVM their metadata
/// names, then the loader. The client files must come first because a truly
/// empty instance has no version metadata for the runtime installer to read;
/// asking for Java first failed on exactly that clean root. A gap that cannot
/// be filled (an unrecognised loader, an unsupported platform for the managed
/// runtime) is an error, so the caller reports it instead of launching into a
/// failure it already knows about.
pub async fn repair<F>(
    client: &reqwest::Client,
    instance: &mut Instance,
    progress: F,
) -> Result<RepairReport, String>
where
    F: Fn(RepairProgress) + Send + Sync + 'static,
{
    let progress = Arc::new(progress);
    let mut report = RepairReport::default();
    let readiness = check(instance);

    for stage in repair_stages(&readiness) {
        match stage {
            RepairStage::GameFiles => {
                let entry = versions::fetch_manifest(client)
                    .await
                    .map_err(|error| format!("could not list Mojang versions: {error}"))?
                    .versions
                    .into_iter()
                    .find(|entry| entry.id == instance.mc_version)
                    .ok_or_else(|| {
                        format!(
                            "Mojang has no version named {}; recreate the instance on a released version",
                            instance.mc_version
                        )
                    })?;
                progress(RepairProgress::Message(format!(
                    "Restoring Minecraft {} files…",
                    instance.mc_version
                )));
                let installer = Arc::clone(&progress);
                vanilla::install_vanilla(client, entry, instance.root.clone(), move |state| {
                    match state {
                        vanilla::InstallProgress::DownloadingLibraries { completed, total }
                        | vanilla::InstallProgress::DownloadingAssets { completed, total } => {
                            installer(RepairProgress::Download { completed, total });
                        }
                        other => installer(RepairProgress::Message(other.message())),
                    }
                })
                .await
                .map_err(|error| format!("game file repair failed: {error}"))?;
                report.game_files = readiness.missing_files;
            }
            RepairStage::Java => {
                progress(RepairProgress::Message(
                    "No Java found — fetching the runtime this version needs…".to_owned(),
                ));
                let installer = Arc::clone(&progress);
                let java = runtime::install_for_instance(client, instance, move |state| {
                    installer(RepairProgress::Message(state.message()));
                })
                .await
                .map_err(|error| format!("Java runtime install failed: {error}"))?;
                instance.java_path = Some(java.clone());
                if let Err(error) = instance.save() {
                    report.notes.push(format!(
                        "runtime installed, but the instance record could not be updated: {error}"
                    ));
                }
                report.java = Some(java);
            }
            RepairStage::Loader => {
                let Some(loader) = readiness.loader.clone() else {
                    continue;
                };
                if !readiness.loader_repairable {
                    return Err(format!(
                        "{loader} cannot be auto-installed; install the loader with its own installer, then press Play"
                    ));
                }
                let label = loader_label(&loader);
                let loader_name = loader.to_ascii_lowercase();
                if matches!(loader_name.as_str(), "forge" | "neoforge") {
                    // Forge and NeoForge publish a Java installer rather than a
                    // plain meta profile, so the JVM that will run the game runs
                    // the install too. The Java stage above has installed one
                    // when none was present.
                    let java = runtime::resolve_java(instance).ok_or_else(|| {
                        format!(
                            "{label} needs a Java runtime to run its installer, and none was found"
                        )
                    })?;
                    progress(RepairProgress::Message(format!(
                        "Installing {label} (its installer downloads the loader)…"
                    )));
                    let install = crate::forge::install(
                        client,
                        &instance.root,
                        &instance.mc_version,
                        &loader_name,
                        &java,
                    )
                    .await
                    .map_err(|error| format!("{label} loader install failed: {error}"))?;
                    report.notes.push(format!(
                        "{label} {} is ready ({} libraries).",
                        install.loader_version, install.libraries
                    ));
                    report.loader = Some(loader);
                } else {
                    progress(RepairProgress::Message(format!("Installing {loader}…")));
                    match crate::loader::provision(
                        client,
                        &instance.root,
                        &instance.mc_version,
                        &loader,
                    )
                    .await
                    {
                        Ok(Some(install)) => {
                            report.notes.push(format!(
                                "{loader} {} is ready ({} libraries).",
                                install.loader_version, install.libraries
                            ));
                            report.loader = Some(loader);
                        }
                        Ok(None) => {
                            return Err(format!("{loader} cannot be auto-installed by Kaelis"));
                        }
                        Err(error) => {
                            return Err(format!("{loader} loader install failed: {error}"));
                        }
                    }
                }
            }
        }
    }

    Ok(report)
}

/// The repair work [`repair`] must do, in launch order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairStage {
    GameFiles,
    Java,
    Loader,
}

fn repair_stages(readiness: &Readiness) -> Vec<RepairStage> {
    let mut stages = Vec::new();
    if readiness.missing_files > 0 {
        stages.push(RepairStage::GameFiles);
    }
    if !readiness.java {
        stages.push(RepairStage::Java);
    }
    if readiness.loader.is_some() {
        stages.push(RepairStage::Loader);
    }
    stages
}

#[cfg(test)]
mod tests {
    use super::{Readiness, RepairStage, check, repair_stages};
    use kaelis_core::instance::Instance;

    fn scratch_instance() -> Instance {
        let root = std::env::temp_dir().join(format!(
            "kaelis-readiness-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        Instance::new("probe", "Probe", "1.21.1", "vanilla", root)
    }

    #[test]
    fn an_empty_instance_is_not_ready_and_says_why() {
        let instance = scratch_instance();
        let readiness = check(&instance);
        assert!(!readiness.ready());
        assert_eq!(readiness.missing_files, 1, "metadata is the first gap");
        assert!(readiness.loader.is_none(), "vanilla needs no loader");
        let issues = readiness.issues();
        assert!(!issues.is_empty());
        assert!(issues.iter().any(|issue| issue.contains("game file")));
        assert!(readiness.summary().starts_with("Repairing "));
        let _ = std::fs::remove_dir_all(&instance.root);
    }

    #[test]
    fn every_managed_loader_is_repairable_and_an_unknown_one_is_reported() {
        let mut instance = scratch_instance();
        for loader in ["fabric", "quilt", "forge", "neoforge"] {
            instance.loader = loader.to_owned();
            let readiness = check(&instance);
            assert_eq!(readiness.loader.as_deref(), Some(loader));
            assert!(readiness.loader_repairable, "{loader} must be installable");
            assert!(
                !readiness
                    .issues()
                    .iter()
                    .any(|issue| issue.contains("own installer")),
                "{loader} must not be reported as manual-only"
            );
        }
        instance.loader = "optifine".to_owned();
        let readiness = check(&instance);
        assert_eq!(readiness.loader.as_deref(), Some("optifine"));
        assert!(!readiness.loader_repairable);
        assert!(
            readiness
                .issues()
                .iter()
                .any(|issue| issue.contains("own installer"))
        );
        let _ = std::fs::remove_dir_all(&instance.root);
    }

    #[test]
    fn a_clean_no_java_forge_instance_restores_files_before_installing_java() {
        let readiness = Readiness {
            java: false,
            missing_files: 1,
            loader: Some("forge".to_owned()),
            loader_repairable: true,
        };
        assert_eq!(
            repair_stages(&readiness),
            vec![
                RepairStage::GameFiles,
                RepairStage::Java,
                RepairStage::Loader
            ],
            "the version metadata needed to choose a JVM must exist first"
        );
        assert!(
            !readiness
                .issues()
                .iter()
                .any(|issue| issue.contains("own installer")),
            "forge must be advertised as self-installing"
        );
    }

    #[test]
    fn a_ready_report_needs_all_three_answers() {
        let ready = Readiness {
            java: true,
            missing_files: 0,
            loader: None,
            loader_repairable: false,
        };
        assert!(ready.ready());
        assert!(ready.issues().is_empty());
        let no_java = Readiness {
            java: false,
            ..ready.clone()
        };
        assert!(!no_java.ready());
        assert!(no_java.issues().iter().any(|issue| issue.contains("Java")));
    }

    /// Full wire test for the report that started this pass: an empty root, no
    /// Java, and a Forge target. Ignored so the normal suite stays offline; run
    /// it with `KAELIS_DATA_DIR` pointed at a scratch directory and `--ignored`.
    #[tokio::test]
    #[ignore = "network: installs a Java runtime, vanilla 1.20.1 and the Forge installer"]
    async fn a_clean_forge_instance_installs_java_and_forge_end_to_end() {
        assert!(
            std::env::var_os("KAELIS_DATA_DIR").is_some(),
            "set KAELIS_DATA_DIR to a scratch directory before running this test"
        );
        let root = std::env::temp_dir().join(format!(
            "kaelis-forge-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        let mut instance = Instance::new("forge-e2e", "Forge E2E", "1.20.1", "forge", root.clone());
        let client = reqwest::Client::builder()
            .user_agent(kaelis_core::brand::user_agent())
            .build()
            .expect("http client");

        let report = super::repair(&client, &mut instance, |_| {})
            .await
            .expect("clean Forge repair");
        assert!(report.java.is_some(), "the managed JVM was installed");
        assert_eq!(report.loader.as_deref(), Some("forge"));

        let readiness = super::check(&instance);
        assert!(readiness.ready(), "{:?}", readiness.issues());
        assert!(
            !readiness
                .issues()
                .iter()
                .any(|issue| issue.contains("own installer")),
            "the old manual-installer status must be gone"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(Instance::instances_dir().join("forge-e2e.json"));
    }
}
