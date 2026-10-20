//! Bounded concurrent downloads with HTTPS-only redirects, streamed writes,
//! size checks, SHA-1 verification, and same-directory replacement.

use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

/// Hard ceiling for any single provider file. Modpacks and shader packs can be
/// large, so this sits well above realistic content while still bounding a
/// hostile server's transfer.
const MAX_DOWNLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ATTEMPTS: usize = 3;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("HTTP {status} while downloading")]
    Status {
        status: reqwest::StatusCode,
        retry_after: Option<std::time::Duration>,
    },
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("hash error: {0}")]
    Hash(#[from] super::hash::HashError),
    #[error("blocking hash worker failed: {0}")]
    Join(String),
    #[error("download URL must use HTTPS: {0}")]
    InsecureUrl(String),
    #[error("invalid SHA-1 value for {0}")]
    InvalidSha1(String),
    #[error("download exceeds the 2 GiB safety limit: {0}")]
    TooLarge(String),
    #[error("download size mismatch for {file}: expected {expected}, got {actual}")]
    SizeMismatch {
        file: String,
        expected: u64,
        actual: u64,
    },
    #[error("hash mismatch for {file}: expected {expected}, got {actual}")]
    HashMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("more than one download targets {0}")]
    DuplicateTarget(String),
}

#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub url: String,
    pub dest: PathBuf,
    /// Expected lowercase hexadecimal SHA-1. Empty disables hash checking.
    pub sha1: String,
    pub expected_size: Option<u64>,
}

impl DownloadTask {
    pub fn new(url: &str, dest: PathBuf, sha1: &str) -> Self {
        Self {
            url: url.to_owned(),
            dest,
            sha1: sha1.to_owned(),
            expected_size: None,
        }
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.expected_size = Some(size);
        self
    }
}

struct StagingFile {
    path: PathBuf,
    committed: bool,
}

impl Drop for StagingFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn staging_path(dest: &Path) -> Result<PathBuf, DownloadError> {
    let parent = dest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .ok_or_else(|| {
            DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "download destination must have a filename",
            ))
        })?
        .to_string_lossy();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{name}.kaelis-{}-{sequence}.part",
        std::process::id()
    )))
}

fn valid_sha1(value: &str) -> bool {
    value.is_empty() || (value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn retryable(error: &DownloadError) -> bool {
    match error {
        DownloadError::Http(error) => {
            crate::retry::retryable_error(error)
                || error.status().is_some_and(crate::retry::retryable_status)
        }
        DownloadError::Status { status, .. } => crate::retry::retryable_status(*status),
        DownloadError::HashMismatch { .. } => true,
        _ => false,
    }
}

async fn sha1_path(path: PathBuf) -> Result<String, DownloadError> {
    tokio::task::spawn_blocking(move || super::hash::sha1_file(&path))
        .await
        .map_err(|error| DownloadError::Join(error.to_string()))?
        .map_err(DownloadError::from)
}

async fn download_once(client: &reqwest::Client, task: &DownloadTask) -> Result<(), DownloadError> {
    let url = reqwest::Url::parse(&task.url)
        .map_err(|error| DownloadError::InsecureUrl(error.to_string()))?;
    if url.scheme() != "https" {
        return Err(DownloadError::InsecureUrl(task.url.clone()));
    }
    if !valid_sha1(&task.sha1) {
        return Err(DownloadError::InvalidSha1(task.dest.display().to_string()));
    }
    if task
        .expected_size
        .is_some_and(|size| size > MAX_DOWNLOAD_BYTES)
    {
        return Err(DownloadError::TooLarge(task.dest.display().to_string()));
    }

    let parent = task
        .dest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let existing_file_is_valid = if tokio::fs::try_exists(&task.dest).await? {
        let size_matches = task.expected_size.is_none_or(|expected| {
            std::fs::metadata(&task.dest).is_ok_and(|metadata| metadata.len() == expected)
        });
        let hash_matches = if task.sha1.is_empty() {
            task.expected_size.is_some() && size_matches
        } else {
            sha1_path(task.dest.clone())
                .await?
                .eq_ignore_ascii_case(&task.sha1)
        };
        size_matches && hash_matches
    } else {
        false
    };
    if existing_file_is_valid {
        return Ok(());
    }

    let mut response = client
        .get(url)
        .header("User-Agent", kaelis_core::brand::user_agent())
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(DownloadError::Status {
            status: response.status(),
            retry_after: crate::retry::retry_after(response.headers()),
        });
    }
    if response.url().scheme() != "https" {
        return Err(DownloadError::InsecureUrl(response.url().to_string()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
    {
        return Err(DownloadError::TooLarge(task.dest.display().to_string()));
    }

    let temp_path = staging_path(&task.dest)?;
    let mut staged = StagingFile {
        path: temp_path.clone(),
        committed: false,
    };
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await?;
    // Batches the small chunks reqwest yields into 64 KiB writes instead of a
    // syscall per chunk; installs move thousands of files.
    let mut file = tokio::io::BufWriter::with_capacity(64 * 1024, file);
    // Hash the bytes as they arrive. Verifying the staged file afterwards would
    // read every downloaded byte a second time; installs move thousands of
    // files, so that pass is the largest avoidable cost of an install.
    let mut hasher = (!task.sha1.is_empty()).then(Sha1::new);
    let mut bytes_written = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        if bytes_written > MAX_DOWNLOAD_BYTES {
            return Err(DownloadError::TooLarge(task.dest.display().to_string()));
        }
        match task.expected_size {
            Some(expected) if bytes_written > expected => {
                return Err(DownloadError::SizeMismatch {
                    file: task.dest.display().to_string(),
                    expected,
                    actual: bytes_written,
                });
            }
            _ => {}
        }
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(&chunk);
        }
        use tokio::io::AsyncWriteExt;
        file.write_all(&chunk).await?;
    }
    use tokio::io::AsyncWriteExt;
    file.flush().await?;
    file.into_inner().sync_all().await?;

    match task.expected_size {
        Some(expected) if bytes_written != expected => {
            return Err(DownloadError::SizeMismatch {
                file: task.dest.display().to_string(),
                expected,
                actual: bytes_written,
            });
        }
        _ => {}
    }
    if let Some(hasher) = hasher {
        let actual = super::hash::hex_encode(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(&task.sha1) {
            return Err(DownloadError::HashMismatch {
                file: task.dest.display().to_string(),
                expected: task.sha1.clone(),
                actual,
            });
        }
    }

    std::fs::rename(&temp_path, &task.dest)?;
    staged.committed = true;
    Ok(())
}

async fn download_one(client: &reqwest::Client, task: &DownloadTask) -> Result<(), DownloadError> {
    for attempt in 0..MAX_ATTEMPTS {
        match download_once(client, task).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 < MAX_ATTEMPTS && retryable(&error) => {
                let delay = match &error {
                    DownloadError::Status {
                        retry_after: Some(delay),
                        ..
                    } => *delay,
                    _ => std::time::Duration::from_millis(250_u64 << attempt),
                };
                tokio::time::sleep(delay.min(crate::retry::MAX_BACKOFF)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(DownloadError::Io(std::io::Error::other(
        "download retry loop ended unexpectedly",
    )))
}

/// Download tasks with bounded parallelism. A caller should use separate
/// destination paths for separate versions of the same mod.
pub async fn download_all(
    client: &reqwest::Client,
    tasks: Vec<DownloadTask>,
    concurrency: usize,
) -> Result<Vec<PathBuf>, DownloadError> {
    download_all_with_progress(client, tasks, concurrency, |_, _| {}).await
}

/// Download tasks with bounded parallelism and a monotonic completion callback.
/// The callback runs once initially with `(0, total)` and after each successful
/// download. A caller should use separate destination paths for separate files.
pub async fn download_all_with_progress<F>(
    client: &reqwest::Client,
    tasks: Vec<DownloadTask>,
    concurrency: usize,
    progress: F,
) -> Result<Vec<PathBuf>, DownloadError>
where
    F: Fn(usize, usize) + Send + Sync + 'static,
{
    let mut destinations = HashSet::with_capacity(tasks.len());
    for task in &tasks {
        if !destinations.insert(task.dest.clone()) {
            return Err(DownloadError::DuplicateTarget(
                task.dest.display().to_string(),
            ));
        }
    }
    let total = tasks.len();
    progress(0, total);
    let completed = std::sync::Arc::new(AtomicU64::new(0));
    let progress = std::sync::Arc::new(progress);
    let results: Vec<Result<PathBuf, DownloadError>> = stream::iter(tasks)
        .map(|task| {
            let completed = std::sync::Arc::clone(&completed);
            let progress = std::sync::Arc::clone(&progress);
            async move {
                download_one(client, &task).await?;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                progress(done as usize, total);
                Ok(task.dest)
            }
        })
        .buffer_unordered(concurrency.clamp(1, 32))
        .collect()
        .await;
    results.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::{DownloadError, retryable};
    use reqwest::StatusCode;
    use std::time::Duration;

    #[test]
    fn retries_rate_limited_and_server_failures_only() {
        assert!(retryable(&DownloadError::Status {
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(Duration::from_secs(1)),
        }));
        assert!(retryable(&DownloadError::Status {
            status: StatusCode::BAD_GATEWAY,
            retry_after: None,
        }));
        assert!(!retryable(&DownloadError::Status {
            status: StatusCode::NOT_FOUND,
            retry_after: None,
        }));
    }
}
