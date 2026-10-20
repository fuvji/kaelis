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
