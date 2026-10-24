//! Background cleanup for stale temporary extraction and staging files.
//! Installed game data and user instance metadata are outside this directory.
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SweeperError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Temp root for native extraction and future download staging.
/// Override with KAELIS_DATA_DIR.
pub fn temp_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("KAELIS_DATA_DIR") {
        return PathBuf::from(dir).join("tmp");
    }
    directories::ProjectDirs::from("gg", "kaelis", "Kaelis")
        .map(|d| d.cache_dir().join("tmp"))
        .unwrap_or_else(|| PathBuf::from("tmp"))
}

/// Remove a finished download / staging dir. Best-effort: missing = ok.
pub fn wipe_path(path: &Path) -> Result<(), SweeperError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Sweep stale temp entries older than `max_age_secs`.
pub fn sweep_stale(max_age_secs: u64) -> Result<usize, SweeperError> {
    let root = temp_root();
    let root_metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Ok(0);
    }
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        let modified = meta.modified().unwrap_or(now);
        let age = now.duration_since(modified).unwrap_or_default().as_secs();
        if age >= max_age_secs {
            let p = entry.path();
            if meta.file_type().is_dir() {
                std::fs::remove_dir_all(&p)?;
            } else {
                std::fs::remove_file(&p)?;
            }
            removed += 1;
        }
    }
    Ok(removed)
}

/// Delete superseded copies of the launcher sitting next to the running one.
///
/// An installer overwrites `Kaelis.exe` in place, but portable folders and
/// manual downloads accumulate `Kaelis-0.1.2.exe`, `Kaelis-0.1.3.exe`, and
/// `Kaelis.exe.old`. A newer build removes every versioned copy older than
/// itself, so upgrading never leaves a second launcher to open by mistake.
/// The running executable and anything newer are never touched.
pub fn retire_older_builds(current_version: &str) -> usize {
    let directory = match std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        Some(directory) => directory,
        None => return 0,
    };
    let running = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
        })
        .unwrap_or_default();
    retire_older_builds_in(&directory, current_version, &running)
}

/// Testable core of [`retire_older_builds`]: `running` is the file name that
/// must survive, compared case-insensitively.
pub fn retire_older_builds_in(directory: &Path, current_version: &str, running: &str) -> usize {
    let Some(current) = version_key(current_version) else {
        return 0;
    };
    let running = running.to_ascii_lowercase();
    let backup = format!("{running}.old");
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name == running {
            continue;
        }
        let stale =
            name == backup || stale_build_version(&name).is_some_and(|version| version < current);
        if !stale {
            continue;
        }
        let path = entry.path();
        let is_file = entry
            .file_type()
            .map(|kind| kind.is_file() || kind.is_symlink())
            .unwrap_or(false);
        if is_file && std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// `Kaelis-0.1.2.exe` → `[0, 1, 2]`. Anything that is not a versioned launcher
/// build name returns `None`, so unrelated files can never match.
fn stale_build_version(name: &str) -> Option<[u32; 3]> {
    let name = name.to_ascii_lowercase();
    let stem = name.strip_suffix(".old").unwrap_or(&name);
    let stem = stem.strip_suffix(".exe").unwrap_or(stem);
    let stem = stem.strip_prefix("kaelis")?;
    let stem = stem.strip_prefix(['-', '_', ' ']).unwrap_or(stem);
    version_key(stem)
}

fn version_key(text: &str) -> Option<[u32; 3]> {
    let mut key = [0u32; 3];
    let mut parts = text.split('.');
    for slot in &mut key {
        *slot = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(key)
}

/// Spawn the periodic background sweeper. Returns the join handle so the
/// launcher can abort it during freeze-on-launch (deep sleep target).
pub fn spawn_background_sweeper(
    runtime: &tokio::runtime::Handle,
    interval_secs: u64,
    max_age_secs: u64,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(1)));
        loop {
            ticker.tick().await;
            let _ = sweep_stale(max_age_secs);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{retire_older_builds_in, stale_build_version};

    #[test]
    fn versioned_build_names_parse_and_others_do_not() {
        assert_eq!(stale_build_version("kaelis-0.1.2.exe"), Some([0, 1, 2]));
        assert_eq!(stale_build_version("Kaelis_1.4.0.exe.old"), Some([1, 4, 0]));
        assert_eq!(stale_build_version("kaelis-notes.exe"), None);
        assert_eq!(stale_build_version("saves.dat"), None);
        assert_eq!(stale_build_version("kaelis.exe"), None);
    }

    #[test]
    fn an_upgrade_removes_older_copies_only() {
        let root = std::env::temp_dir().join(format!(
            "kaelis-retire-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");
        for name in [
            "Kaelis.exe",
            "Kaelis.exe.old",
            "Kaelis-0.1.3.exe",
            "Kaelis-0.1.4.exe",
            "Kaelis-notes.exe",
            "saves.dat",
        ] {
            std::fs::write(root.join(name), b"x").expect("fixture file");
        }
        // 0.1.3 is older, and the .old backup of the running name is dead.
        assert_eq!(retire_older_builds_in(&root, "0.1.4", "kaelis.exe"), 2);
        assert!(root.join("Kaelis.exe").exists());
        assert!(root.join("Kaelis-0.1.4.exe").exists());
        assert!(root.join("Kaelis-notes.exe").exists());
        assert!(root.join("saves.dat").exists());
        assert!(!root.join("Kaelis-0.1.3.exe").exists());
        assert!(!root.join("Kaelis.exe.old").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
