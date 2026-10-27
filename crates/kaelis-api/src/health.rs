//! Pre-launch readiness and automatic repair.
//!
//! A launch used to fail wherever a piece was missing: a formatted machine had
//! no Java, a half-finished download had no client jar, and a deleted library
//! only surfaced as a stack trace inside the game's log. [`check`] answers the
//! question the Play button actually asks — can this instance start right now?
//! — and [`repair`] fills every gap it can reach from Mojang's own metadata:
//! the version-matched Java runtime, the missing client files, and a
//! Fabric-family loader.
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
    /// Whether [`repair`] can install that loader on its own. Forge and
    /// NeoForge need their own installer, so those are reported, not guessed.
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
            matches!(loader_name.as_str(), "fabric" | "quilt"),
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
/// Steps run in the order a launch needs them: a JVM, then the client files,
/// then the loader. A gap that cannot be filled (a Forge loader, an unsupported
/// platform for the managed runtime) is an error, so the caller reports it
/// instead of launching into a failure it already knows about.
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

    if !readiness.java {
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

    if readiness.missing_files > 0 {
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
        vanilla::install_vanilla(
            client,
            entry,
            instance.root.clone(),
            move |state| match state {
                vanilla::InstallProgress::DownloadingLibraries { completed, total }
                | vanilla::InstallProgress::DownloadingAssets { completed, total } => {
                    installer(RepairProgress::Download { completed, total });
                }
                other => installer(RepairProgress::Message(other.message())),
            },
        )
        .await
        .map_err(|error| format!("game file repair failed: {error}"))?;
        report.game_files = readiness.missing_files;
    }

    if let Some(loader) = readiness.loader {
        if !readiness.loader_repairable {
            return Err(format!(
                "{loader} cannot be auto-installed; import the pack or install the loader, then press Play"
            ));
        }
        progress(RepairProgress::Message(format!("Installing {loader}…")));
        match crate::loader::provision(client, &instance.root, &instance.mc_version, &loader).await
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
            Err(error) => return Err(format!("{loader} loader install failed: {error}")),
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{Readiness, check};
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
    fn a_missing_fabric_loader_is_repairable_and_forge_is_reported() {
        let mut instance = scratch_instance();
        instance.loader = "fabric".to_owned();
        let readiness = check(&instance);
        assert_eq!(readiness.loader.as_deref(), Some("fabric"));
        assert!(readiness.loader_repairable);
        instance.loader = "forge".to_owned();
        let readiness = check(&instance);
        assert_eq!(readiness.loader.as_deref(), Some("forge"));
        assert!(!readiness.loader_repairable);
        assert!(
            readiness
                .issues()
                .iter()
                .any(|issue| issue.contains("installer"))
        );
        let _ = std::fs::remove_dir_all(&instance.root);
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
}
