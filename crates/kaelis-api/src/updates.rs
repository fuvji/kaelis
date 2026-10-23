//! Mod update checking through Modrinth's bulk hash endpoint.
//!
//! Every installed jar is hashed once and the hashes are sent to
//! `POST /v2/version_files/update`, which answers with the newest compatible
//! version for each file in one request. A jar whose current hash matches a
//! file of that version is up to date. Jars from other providers simply do not
//! appear in the answer and are reported as unknown instead of as updates.
//!
//! SHA-1 results are cached by path, length, and modification time, so a second
//! check of an unchanged instance does no disk reads.

use crate::modrinth::{MODRINTH_API, ModVersion, ProjectInfo};
use crate::mods::ContentKind;
use kaelis_core::instance::Instance;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;
use thiserror::Error;

/// Modrinth is asked to resolve every jar in one request; this keeps a hostile
/// or accidental `mods/` folder of thousands of files from producing a giant
/// body. Files beyond the cap are reported as skipped.
const MAX_HASHES: usize = 500;
const MAX_CACHE_ENTRIES: usize = 4096;
const HASH_CONCURRENCY: usize = 4;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hash: {0}")]
    Hash(#[from] crate::hash::HashError),
    #[error("could not hash installed files: {0}")]
    Join(String),
    #[error("Modrinth returned an unsafe file name: {0}")]
    UnsafeFile(String),
    #[error("download failed: {0}")]
    Download(#[from] crate::downloader::DownloadError),
}

/// One jar whose project has a newer compatible version.
#[derive(Debug, Clone, Serialize)]
pub struct ModUpdate {
    pub project_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// File currently on disk, including a `.disabled` suffix.
    pub current_file: String,
    pub latest_version: String,
    /// `release`, `beta`, or `alpha` for the offered version.
    pub channel: String,
    pub latest_file: String,
    pub url: String,
    pub sha1: String,
    pub size: u64,
    /// Whether the old file was disabled; the replacement keeps that state.
    pub disabled: bool,
}

/// What one check found.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateReport {
    /// Installable files that were sent to Modrinth.
    pub checked: usize,
    /// Files already at the newest compatible version.
    pub up_to_date: usize,
    /// Files Modrinth does not recognize, or has no compatible version for.
    pub unmatched: usize,
    /// Files left out because the request cap was reached.
    pub skipped: usize,
    pub updates: Vec<ModUpdate>,
}

impl UpdateReport {
    pub fn summary(&self) -> String {
        if self.updates.is_empty() {
            return format!(
                "All {} checked mod{} are current.",
                self.up_to_date + self.unmatched,
                if self.up_to_date + self.unmatched == 1 {
                    ""
                } else {
                    "s"
                }
            );
        }
        format!(
            "{} update{} available ({} checked).",
            self.updates.len(),
            if self.updates.len() == 1 { "" } else { "s" },
            self.checked
        )
    }
}

struct CachedHash {
    len: u64,
    modified: Option<SystemTime>,
    sha1: String,
}

static HASH_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedHash>>> = OnceLock::new();

/// SHA-1 of one file, reused while its length and modification time match.
pub fn sha1_cached(path: &Path) -> Result<String, crate::hash::HashError> {
    let metadata = std::fs::metadata(path)?;
    let len = metadata.len();
    let modified = metadata.modified().ok();
    let cache = HASH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(entries) = cache.lock()
        && let Some(entry) = entries.get(path)
        && entry.len == len
        && entry.modified == modified
    {
        return Ok(entry.sha1.clone());
    }
    let sha1 = crate::hash::sha1_file(path)?;
    // A file modified in the last two seconds is not cached: some filesystems
    // round timestamps coarsely enough that two same-length writes can share
    // one, and an installer that has just replaced a jar would otherwise be
    // reported with its previous hash on the next check.
    let settled = modified
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= std::time::Duration::from_secs(2));
    if settled && let Ok(mut entries) = cache.lock() {
        if entries.len() >= MAX_CACHE_ENTRIES {
            entries.clear();
        }
        entries.insert(
            path.to_path_buf(),
            CachedHash {
                len,
                modified,
                sha1: sha1.clone(),
            },
        );
    }
    Ok(sha1)
}

/// Files in `mods/` that can be checked, with their current hash.
async fn hash_installed(
    locals: &[crate::modmeta::LocalMod],
) -> Result<Vec<(PathBuf, String, bool)>, UpdateError> {
    let files = locals
        .iter()
        .take(MAX_HASHES)
        .map(|local| (local.path.clone(), local.disabled))
        .collect::<Vec<_>>();
    use futures::StreamExt;
    let hashed = futures::stream::iter(files)
        .map(|(path, disabled)| async move {
            let hashed = tokio::task::spawn_blocking({
                let path = path.clone();
                move || sha1_cached(&path)
            })
            .await
            .map_err(|error| UpdateError::Join(error.to_string()))??;
            Ok::<_, UpdateError>((path, hashed, disabled))
        })
        .buffer_unordered(HASH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    hashed.into_iter().collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether a provider-supplied file name stays inside `mods/`.
fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains('/')
        && !name.contains('\\')
        && Path::new(name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && name.to_ascii_lowercase().ends_with(".jar")
}

/// The newest compatible version for each installed file, keyed by hash.
async fn latest_versions(
    client: &reqwest::Client,
    hashes: &[String],
    mc_version: &str,
    loader: &str,
    include_prereleases: bool,
) -> Result<HashMap<String, ModVersion>, UpdateError> {
    let mut body = serde_json::json!({
        "hashes": hashes,
        "algorithm": "sha1",
        "loaders": [loader.to_ascii_lowercase()],
        "game_versions": [mc_version],
    });
    // Stable builds only by default: offering a beta as a routine update is
    // surprising. A jar that is already on a prerelease channel asks again
    // with the wider set, so beta users still get their channel.
    if !include_prereleases {
        body["version_types"] = serde_json::json!(["release"]);
    }
    let url = format!("{MODRINTH_API}/version_files/update");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .post(&url)
            .header("User-Agent", kaelis_core::brand::user_agent())
            .header("Accept", "application/json")
            .json(&body)
    })
    .await?;
    Ok(response
        .error_for_status()?
        .json::<HashMap<String, ModVersion>>()
        .await?)
}

/// Project titles for the rows that need them. A lookup failure degrades to the
/// project id rather than discarding an otherwise valid update list.
async fn project_metadata(
    client: &reqwest::Client,
    ids: &[String],
) -> HashMap<String, ProjectInfo> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let encoded = match serde_json::to_string(ids) {
        Ok(encoded) => encoded,
        Err(_) => return HashMap::new(),
    };
    let url = format!("{MODRINTH_API}/projects");
    let response = crate::retry::get_with_retry(&crate::retry::RetryPolicy::default(), || {
        client
            .get(&url)
            .header("User-Agent", kaelis_core::brand::user_agent())
            .header("Accept", "application/json")
            .query(&[("ids", encoded.as_str())])
    })
    .await;
    let Ok(response) = response else {
        return HashMap::new();
    };
    let Ok(response) = response.error_for_status() else {
        return HashMap::new();
    };
    let projects = response
        .json::<Vec<ProjectInfo>>()
        .await
        .unwrap_or_default();
    projects
        .into_iter()
        .map(|project| (project.id.clone(), project))
        .collect()
}

/// Build a report from the endpoint answer. Separated from the network so the
/// comparison rules can be tested without a live server.
fn build_report(
    files: &[(PathBuf, String, bool)],
    versions: &HashMap<String, ModVersion>,
    projects: &HashMap<String, ProjectInfo>,
) -> UpdateReport {
    let mut report = UpdateReport {
        checked: files.len(),
        ..UpdateReport::default()
    };
    for (path, hash, disabled) in files {
        let Some(version) = versions.get(hash) else {
            report.unmatched += 1;
            continue;
        };
        if version
            .files
            .iter()
            .any(|file| file.hashes.sha1.eq_ignore_ascii_case(hash))
        {
            report.up_to_date += 1;
            continue;
        }
        let Some(file) = version
            .files
            .iter()
            .find(|file| file.primary)
            .or_else(|| version.files.first())
        else {
            report.unmatched += 1;
            continue;
        };
        if !safe_file_name(&file.filename) || file.url.is_empty() {
            report.unmatched += 1;
            continue;
        }
        let project = projects.get(&version.project_id);
        let title = project
            .map(|project| project.title.clone())
            .unwrap_or_else(|| version.project_id.clone());
        report.updates.push(ModUpdate {
            project_id: version.project_id.clone(),
            title,
            slug: project.and_then(|project| project.slug.clone()),
            current_file: file_name(path),
            latest_version: version.version_number.clone(),
            channel: if version.version_type.is_empty() {
                "release".to_owned()
            } else {
                version.version_type.clone()
            },
            latest_file: file.filename.clone(),
            url: file.url.clone(),
            sha1: file.hashes.sha1.clone(),
            size: file.size,
            disabled: *disabled,
        });
    }
    report.updates.sort_by(|left, right| {
        left.title
            .to_ascii_lowercase()
            .cmp(&right.title.to_ascii_lowercase())
    });
    report
}

/// Check every installed mod on `instance` against Modrinth.
pub async fn check_updates(
    client: &reqwest::Client,
    instance: &Instance,
) -> Result<UpdateReport, UpdateError> {
    let mods_dir = instance.root.join(ContentKind::Mod.directory());
    let all = crate::modmeta::scan_dir(&mods_dir);
    let skipped = all.len().saturating_sub(MAX_HASHES);
    let files = hash_installed(&all).await?;
    if files.is_empty() {
        return Ok(UpdateReport {
            skipped,
            ..UpdateReport::default()
        });
    }
    let hashes = files
        .iter()
        .map(|(_, hash, _)| hash.clone())
        .collect::<Vec<_>>();
    let mut versions = latest_versions(
        client,
        &hashes,
        &instance.mc_version,
        &instance.loader,
        false,
    )
    .await?;
    // A release-only answer can be older than a jar the player installed from
    // the beta channel. Ask those jars again with every channel enabled.
    let mut retry = Vec::new();
    for (_, hash, _) in &files {
        let Some(version) = versions.get(hash) else {
            continue;
        };
        let already_current = version
            .files
            .iter()
            .any(|file| file.hashes.sha1.eq_ignore_ascii_case(hash));
        if already_current {
            continue;
        }
        if let Ok(current) = crate::modrinth::version_file(client, hash).await
            && !current.version_type.eq_ignore_ascii_case("release")
        {
            retry.push(hash.clone());
        }
        if retry.len() >= 24 {
            break;
        }
    }
    if !retry.is_empty() {
        let widened =
            latest_versions(client, &retry, &instance.mc_version, &instance.loader, true).await?;
        for (hash, version) in widened {
            versions.insert(hash, version);
        }
    }
    let mut ids = versions
        .values()
        .map(|version| version.project_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    let projects = project_metadata(client, &ids).await;
    let mut report = build_report(&files, &versions, &projects);
    report.skipped = skipped;
    Ok(report)
}

/// Download and install one update, replacing the old jar only after the new
/// file passed its hash check.
pub async fn apply_update(
    client: &reqwest::Client,
    mods_dir: &Path,
    update: &ModUpdate,
) -> Result<PathBuf, UpdateError> {
    if !safe_file_name(&update.latest_file) {
        return Err(UpdateError::UnsafeFile(update.latest_file.clone()));
    }
    let latest_name = if update.disabled {
        format!("{}.disabled", update.latest_file)
    } else {
        update.latest_file.clone()
    };
    let target = mods_dir.join(&latest_name);
    let task = crate::downloader::DownloadTask::new(&update.url, target.clone(), &update.sha1)
        .with_size(update.size);
    crate::downloader::download_all(client, vec![task], 1).await?;
    let previous = mods_dir.join(&update.current_file);
    if previous != target && previous.is_file() {
        std::fs::remove_file(&previous)?;
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::{ModUpdate, UpdateReport, build_report, safe_file_name, sha1_cached};
    use crate::modrinth::{FileHashes, ModVersion, ModVersionFile, ProjectInfo};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    fn version(project: &str, number: &str, file: &str, sha1: &str) -> ModVersion {
        ModVersion {
            id: "version".to_owned(),
            project_id: project.to_owned(),
            version_number: number.to_owned(),
            game_versions: vec!["1.21.1".to_owned()],
            loaders: vec!["fabric".to_owned()],
            version_type: "release".to_owned(),
            files: vec![ModVersionFile {
                url: format!("https://cdn.example/{file}"),
                filename: file.to_owned(),
                primary: true,
                size: 1024,
                hashes: FileHashes {
                    sha1: sha1.to_owned(),
                    sha512: String::new(),
                },
            }],
            dependencies: Vec::new(),
        }
    }

    fn file(name: &str, hash: &str, disabled: bool) -> (PathBuf, String, bool) {
        (
            PathBuf::from(format!("/mods/{name}")),
            hash.to_owned(),
            disabled,
        )
    }

    fn project(id: &str, title: &str, slug: &str) -> ProjectInfo {
        ProjectInfo {
            id: id.to_owned(),
            slug: Some(slug.to_owned()),
            title: title.to_owned(),
            description: String::new(),
            downloads: 0,
            icon_url: None,
            project_type: Some("mod".to_owned()),
        }
    }

    #[test]
    fn an_unchanged_hash_is_up_to_date() {
        let files = vec![file("sodium.jar", "aaa", false)];
        let versions = HashMap::from([(
            "aaa".to_owned(),
            version("sodium", "0.5", "sodium.jar", "aaa"),
        )]);
        let report = build_report(&files, &versions, &HashMap::new());
        assert_eq!(report.checked, 1);
        assert_eq!(report.up_to_date, 1);
        assert!(report.updates.is_empty());
    }

    #[test]
    fn a_newer_file_becomes_an_update_with_its_title() {
        let files = vec![file("sodium-0.4.jar", "old", true)];
        let versions = HashMap::from([(
            "old".to_owned(),
            version("AANobbMI", "0.5.9", "sodium-0.5.9.jar", "new"),
        )]);
        let projects = HashMap::from([(
            "AANobbMI".to_owned(),
            project("AANobbMI", "Sodium", "sodium"),
        )]);
        let report = build_report(&files, &versions, &projects);
        assert_eq!(report.updates.len(), 1);
        let update = &report.updates[0];
        assert_eq!(update.title, "Sodium");
        assert_eq!(update.slug.as_deref(), Some("sodium"));
        assert_eq!(update.current_file, "sodium-0.4.jar");
        assert_eq!(update.latest_file, "sodium-0.5.9.jar");
        assert_eq!(update.latest_version, "0.5.9");
        // A disabled jar is replaced by a disabled jar.
        assert!(update.disabled);
        assert_eq!(report.up_to_date, 0);
    }

    #[test]
    fn a_jar_modrinth_does_not_know_is_unmatched_not_an_update() {
        let files = vec![file("private.jar", "zzz", false)];
        let report = build_report(&files, &HashMap::new(), &HashMap::new());
        assert_eq!(report.unmatched, 1);
        assert!(report.updates.is_empty());
        assert!(report.summary().contains("current"));
    }

    #[test]
    fn the_summary_counts_updates() {
        let report = UpdateReport {
            checked: 3,
            updates: vec![ModUpdate {
                project_id: "x".to_owned(),
                title: "X".to_owned(),
                slug: None,
                current_file: "x.jar".to_owned(),
                latest_version: "2".to_owned(),
                channel: "release".to_owned(),
                latest_file: "x-2.jar".to_owned(),
                url: "https://cdn.example/x-2.jar".to_owned(),
                sha1: "abc".to_owned(),
                size: 1,
                disabled: false,
            }],
            ..UpdateReport::default()
        };
        assert_eq!(report.summary(), "1 update available (3 checked).");
    }

    #[test]
    fn provider_file_names_cannot_escape_the_mods_folder() {
        for name in [
            "../evil.jar",
            "..\\evil.jar",
            "sub/evil.jar",
            "sub\\evil.jar",
            "evil.zip",
            "",
        ] {
            assert!(!safe_file_name(name), "{name}");
        }
        assert!(safe_file_name("sodium-0.5.9.jar"));
        assert!(safe_file_name("Sodium.jar"));
    }

    #[test]
    fn repeated_hashes_reuse_the_cache_entry() {
        let dir = std::env::temp_dir().join(format!(
            "kaelis-update-hash-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mod.jar");
        std::fs::write(&path, b"jar bytes").unwrap();
        let first = sha1_cached(&path).unwrap();
        let second = sha1_cached(Path::new(&path)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 40);
        // A just-written file is not trusted from the cache; replacing it with
        // different bytes must produce a different hash immediately.
        std::fs::write(&path, b"new bytes").unwrap();
        let third = sha1_cached(&path).unwrap();
        assert_ne!(first, third);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
