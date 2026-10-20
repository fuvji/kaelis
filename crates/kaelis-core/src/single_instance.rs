//! Process-wide single-instance guard. The operating system releases the
//! handle/lock automatically if Kaelis exits unexpectedly.

#[cfg(unix)]
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SingleInstanceError {
    #[error("another Kaelis instance is already running ({0})")]
    AlreadyRunning(String),
    #[error("single-instance guard failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Held for the full process lifetime. Dropping it releases the OS guard.
#[derive(Debug)]
pub struct SingleInstance {
    description: String,
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    file: std::fs::File,
}

impl SingleInstance {
    /// Windows mutex name. The default install keeps one launcher per session;
    /// an explicit `KAELIS_DATA_DIR` (portable copies, tests) gets its own
    /// guard so two independent data roots can run side by side.
    #[cfg(windows)]
    fn mutex_name() -> String {
        const BASE: &str = "Local\\KaelisLauncher";
        let Some(dir) = std::env::var_os("KAELIS_DATA_DIR") else {
            return BASE.to_owned();
        };
        // FNV-1a over the lowercased path: short, stable, and cheap.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in std::path::PathBuf::from(dir)
            .to_string_lossy()
            .to_lowercase()
            .bytes()
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("Local\\KaelisLauncher-{hash:016x}")
    }

    #[cfg(unix)]
    fn lock_path() -> PathBuf {
        if let Some(dir) = std::env::var_os("KAELIS_DATA_DIR") {
            return PathBuf::from(dir).join("kaelis.lock");
        }
        directories::ProjectDirs::from("gg", "kaelis", "Kaelis")
            .map(|dirs| dirs.data_dir().join("kaelis.lock"))
            .unwrap_or_else(|| PathBuf::from("kaelis.lock"))
    }

    /// Claim a named Windows mutex or an exclusive Unix advisory lock.
    pub fn claim() -> Result<Self, SingleInstanceError> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::{
                CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, SetLastError,
            };
            use windows_sys::Win32::System::Threading::CreateMutexW;

            let description = Self::mutex_name();
            let name: Vec<u16> = std::ffi::OsStr::new(&description)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // Clear stale thread-local error state so only this call determines
            // whether the named mutex already existed.
            unsafe { SetLastError(0) };
            // SAFETY: `name` is NUL terminated and remains alive through the call.
            let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
            if handle.is_null() {
                return Err(SingleInstanceError::Io(std::io::Error::last_os_error()));
            }
            // CreateMutexW returns a valid handle for both a new and existing mutex.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe { CloseHandle(handle) };
                return Err(SingleInstanceError::AlreadyRunning(description));
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

            let path = Self::lock_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)?;
            // LOCK_EX | LOCK_NB: the kernel releases the lock when this fd closes.
            // SAFETY: the fd is owned by `file` and remains open for the guard's lifetime.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if matches!(error.kind(), std::io::ErrorKind::WouldBlock) {
                    return Err(SingleInstanceError::AlreadyRunning(
                        path.display().to_string(),
                    ));
                }
                return Err(SingleInstanceError::Io(error));
            }
            Ok(Self {
                description: path.display().to_string(),
                file,
            })
        }

        #[cfg(not(any(windows, unix)))]
        {
            Err(SingleInstanceError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Kaelis single-instance guard is not implemented for this target",
            )))
        }
    }

    pub fn description(&self) -> &str {
        #[cfg(unix)]
        let _keep_file_open = &self.file;
        &self.description
    }
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        // SAFETY: this handle was returned by CreateMutexW and is owned here.
        unsafe {
            ReleaseMutex(self.handle);
            CloseHandle(self.handle);
        }
    }
}
