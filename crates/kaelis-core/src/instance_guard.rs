//! Per-instance launch guard.
//!
//! The launcher itself is single-instance, but one launcher can run several
//! different instances at once. Two JVMs writing the same world, `options.txt`,
//! or logs is the case that must never happen, so every launch claims a guard
//! named after the instance. A second launch of the same instance fails with a
//! clear message while a launch of a different instance proceeds.
//!
//! The operating system owns the guard: a named mutex on Windows and an
//! exclusive advisory lock on a per-data-directory file on Unix. A crashed
//! launcher therefore cannot leave a stale lock behind.

use crate::instance::Instance;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InstanceGuardError {
    #[error("another launch of this instance is already running ({0})")]
    AlreadyRunning(String),
    #[error("instance launch guard failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Held for the lifetime of one game process. Dropping it releases the guard.
#[derive(Debug)]
pub struct InstanceGuard {
    description: String,
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    file: std::fs::File,
}

// The Windows guard is a kernel mutex object, not a thread-owned critical
// section: spawning happens on a worker and the handle may be closed from any
// thread once the process is gone. The Unix guard is an owned file descriptor.
unsafe impl Send for InstanceGuard {}

impl InstanceGuard {
    /// Claim the guard for `instance_id` under the live data root.
    pub fn claim(instance_id: &str) -> Result<Self, InstanceGuardError> {
        Self::claim_in(instance_id, &data_dir())
    }

    /// Claim the guard under an explicit data root. Importing a portable copy
    /// or testing a second data root uses this so the name can never collide
    /// with the default installation.
    pub fn claim_in(instance_id: &str, root: &Path) -> Result<Self, InstanceGuardError> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::{
                CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, SetLastError,
            };
            use windows_sys::Win32::System::Threading::CreateMutexW;

            let description = mutex_name(instance_id, root);
            let name: Vec<u16> = std::ffi::OsStr::new(&description)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            unsafe { SetLastError(0) };
            // SAFETY: `name` is NUL terminated and outlives the call.
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if handle.is_null() {
                return Err(InstanceGuardError::Io(std::io::Error::last_os_error()));
            }
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe { CloseHandle(handle) };
                return Err(InstanceGuardError::AlreadyRunning(description));
            }
            Ok(Self {
                description,
                handle,
            })
        }

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::OpenOptionsExt;

            let path = lock_path(instance_id, root);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)?;
            // SAFETY: the fd is owned by `file` and stays open for the guard.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if matches!(error.kind(), std::io::ErrorKind::WouldBlock) {
                    return Err(InstanceGuardError::AlreadyRunning(
                        path.display().to_string(),
                    ));
                }
                return Err(InstanceGuardError::Io(error));
            }
            Ok(Self {
                description: path.display().to_string(),
                file,
            })
        }

        #[cfg(not(any(windows, unix)))]
        {
            let _ = (instance_id, root);
            Err(InstanceGuardError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "per-instance launch guards are not implemented for this target",
            )))
        }
    }

    pub fn description(&self) -> &str {
        #[cfg(unix)]
        let _keep_file_open = &self.file;
        &self.description
    }
}

fn data_dir() -> PathBuf {
    let instances = Instance::instances_dir();
    instances
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(instances)
}

#[cfg(windows)]
fn mutex_name(instance_id: &str, root: &Path) -> String {
    // The data root is part of the name so two portable copies with equal
    // instance ids do not block each other.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(root.to_string_lossy().to_lowercase().as_bytes());
    feed(b"\0");
    feed(instance_id.to_ascii_lowercase().as_bytes());
    format!("Local\\KaelisInstance-{hash:016x}")
}

#[cfg(unix)]
fn lock_path(instance_id: &str, root: &Path) -> PathBuf {
    root.join("locks")
        .join(format!("{}.lock", sanitize_id(instance_id)))
}

#[cfg(unix)]
fn sanitize_id(instance_id: &str) -> String {
    instance_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect()
}

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        // SAFETY: the handle came from CreateMutexW and is owned here.
        unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::InstanceGuard;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kaelis-instance-guard-{tag}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn different_instances_run_beside_each_other() {
        let root = scratch("different");
        let first = InstanceGuard::claim_in("fabric-1-21", &root).expect("first");
        let second = InstanceGuard::claim_in("fabric-1-21-sodium", &root).expect("second");
        drop(second);
        drop(first);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_data_roots_do_not_share_a_guard() {
        let a = scratch("root-a");
        let b = scratch("root-b");
        let guard_a = InstanceGuard::claim_in("shared", &a).expect("root a");
        let guard_b = InstanceGuard::claim_in("shared", &b).expect("root b");
        assert_ne!(guard_a.description(), guard_b.description());
        drop(guard_b);
        drop(guard_a);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[cfg(windows)]
    #[test]
    fn the_same_instance_cannot_be_launched_twice() {
        let root = scratch("same");
        let first = InstanceGuard::claim_in("fabric-1-21", &root).expect("first claim");
        let second = InstanceGuard::claim_in("fabric-1-21", &root);
        assert!(second.is_err(), "a second launch must be refused");
        drop(first);
        let third = InstanceGuard::claim_in("fabric-1-21", &root).expect("reclaim after exit");
        drop(third);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_reusable_after_the_guard_drops() {
        let root = scratch("reuse");
        let first = InstanceGuard::claim_in("fabric-1-21", &root).expect("first");
        drop(first);
        let second = InstanceGuard::claim_in("fabric-1-21", &root).expect("reclaim");
        drop(second);
        let _ = std::fs::remove_dir_all(&root);
    }
}
