//! Bounded single-instance argument handoff.
//!
//! A second Kaelis process cannot open a second launcher, but it still has to
//! deliver the arguments it was started with to the process that owns the
//! single-instance guard. Each launch is one length-prefixed JSON array of
//! arguments; the receiver is a per-user local channel (Windows named pipe,
//! Unix domain socket) with a hard payload cap.

use std::sync::mpsc::Receiver;
use std::time::Duration;
use thiserror::Error;

/// Largest accepted handoff payload. A normal launch is far smaller; the cap
/// keeps a stuck or hostile local writer from growing the listener's memory.
pub const MAX_HANDOFF_BYTES: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum HandoffError {
    #[error("the primary Kaelis process is not accepting handoffs")]
    NotRunning,
    #[error("handoff payload is {0} bytes; the limit is {MAX_HANDOFF_BYTES}")]
    TooLarge(usize),
    #[error("handoff payload is not a valid argument list: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("handoff I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Listener owned by the primary Kaelis process. Dropping it stops the
/// receiver channel; the OS endpoint is released when the process exits.
pub struct HandoffServer {
    name: String,
    receiver: Receiver<Vec<String>>,
}

impl HandoffServer {
    /// Bind the channel for the current user and data directory.
    pub fn bind() -> Result<Self, HandoffError> {
        Self::bind_named(&handoff_name())
    }

    /// Bind an explicit channel name. Used by tests to avoid colliding with a
    /// running launcher.
    pub fn bind_named(name: &str) -> Result<Self, HandoffError> {
        platform::bind(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Blocking receive, for the GUI bridge thread.
    pub fn recv(&self) -> Option<Vec<String>> {
        self.receiver.recv().ok()
    }

    pub fn try_recv(&self) -> Option<Vec<String>> {
        self.receiver.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<Vec<String>> {
        self.receiver.recv_timeout(timeout).ok()
    }
}

/// Forward arguments to the primary process on the current user's channel.
pub fn forward(arguments: &[String]) -> Result<(), HandoffError> {
    forward_to(&handoff_name(), arguments)
}

/// Forward arguments to an explicit channel name.
pub fn forward_to(name: &str, arguments: &[String]) -> Result<(), HandoffError> {
    let payload = serde_json::to_vec(arguments)?;
    if payload.len() > MAX_HANDOFF_BYTES {
        return Err(HandoffError::TooLarge(payload.len()));
    }
    platform::send(name, &payload)
}

/// Stable per-user, per-data-directory channel name. FNV-1a keeps the derived
/// name short and identical across launches without persisting a token.
pub fn handoff_name() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "local-user".to_owned());
    let data_dir = std::env::var_os("KAELIS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            directories::ProjectDirs::from("gg", "kaelis", "Kaelis")
                .map(|dirs| dirs.data_dir().to_path_buf())
        })
        .unwrap_or_default();
    let scope = format!("{user}\u{0}{}", data_dir.display());
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in scope.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("kaelis-handoff-{hash:016x}")
}

#[cfg(windows)]
mod platform {
    use super::{HandoffError, MAX_HANDOFF_BYTES};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use std::sync::mpsc;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
        GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
        ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        WaitNamedPipeW,
    };

    /// Owns a Win32 handle so every error path releases it.
    struct Pipe(HANDLE);

    // SAFETY: a Win32 handle is process-wide and only used by the thread that
    // owns this value; the listener moves it once into its serving thread.
    unsafe impl Send for Pipe {}

    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateNamedPipeW/CreateFileW and is
            // owned by this value.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn pipe_path(name: &str) -> String {
        format!(r"\\.\pipe\{name}")
    }

    fn wide(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn bind(name: &str) -> Result<super::HandoffServer, HandoffError> {
        let path = wide(&pipe_path(name));
        // SAFETY: `path` is NUL terminated and lives through the call. A null
        // security descriptor applies the process token's default DACL, and
        // PIPE_REJECT_REMOTE_CLIENTS keeps the endpoint local.
        let handle = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                MAX_HANDOFF_BYTES as u32,
                MAX_HANDOFF_BYTES as u32,
                0,
                ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let pipe = Pipe(handle);
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("kaelis-handoff".to_owned())
            .spawn(move || serve(pipe, sender))?;
        Ok(super::HandoffServer {
            name: name.to_owned(),
            receiver,
        })
    }

    fn serve(pipe: Pipe, sender: mpsc::Sender<Vec<String>>) {
        loop {
            // SAFETY: `pipe.0` is a live server handle. A blocking connect is
            // fine on this dedicated thread.
            let connected = unsafe { ConnectNamedPipe(pipe.0, ptr::null_mut()) } != 0;
            if !connected {
                match unsafe { GetLastError() } {
                    // The client connected between create and connect.
                    ERROR_PIPE_CONNECTED => {}
                    // The client vanished before completing the connection.
                    ERROR_NO_DATA => {
                        unsafe { DisconnectNamedPipe(pipe.0) };
                        continue;
                    }
                    _ => break,
                }
            }
            if let Some(arguments) = read_message(pipe.0)
                && sender.send(arguments).is_err()
            {
                break;
            }
            // SAFETY: the handle is still owned by `pipe`.
            unsafe { DisconnectNamedPipe(pipe.0) };
        }
    }

    fn read_message(handle: HANDLE) -> Option<Vec<String>> {
        let mut header = [0_u8; 4];
        read_exact(handle, &mut header).ok()?;
        let length = u32::from_be_bytes(header) as usize;
        if length > MAX_HANDOFF_BYTES {
            return None;
        }
        let mut payload = vec![0_u8; length];
        read_exact(handle, &mut payload).ok()?;
        serde_json::from_slice(&payload).ok()
    }

    fn read_exact(handle: HANDLE, buffer: &mut [u8]) -> std::io::Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            let mut read = 0_u32;
            // SAFETY: the buffer and out-parameter are valid for the call and
            // the pipe is a synchronous byte-mode handle.
            let ok = unsafe {
                ReadFile(
                    handle,
                    buffer[offset..].as_mut_ptr(),
                    (buffer.len() - offset) as u32,
                    &mut read,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if read == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            offset += read as usize;
        }
        Ok(())
    }

    fn write_all(handle: HANDLE, buffer: &[u8]) -> std::io::Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            let mut written = 0_u32;
            // SAFETY: the buffer is valid for the call; synchronous pipe write.
            let ok = unsafe {
                WriteFile(
                    handle,
                    buffer[offset..].as_ptr(),
                    (buffer.len() - offset) as u32,
                    &mut written,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if written == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            offset += written as usize;
        }
        Ok(())
    }

    pub(super) fn send(name: &str, payload: &[u8]) -> Result<(), HandoffError> {
        let path = wide(&pipe_path(name));
        let mut pipe = None;
        let mut last_error = 0_u32;
        // The primary creates this endpoint moments after it owns the guard, so
        // a double-click during a cold start can arrive first. Two seconds of
        // retries covers that window without making a real second launch wait.
        const ATTEMPTS: usize = 40;
        for attempt in 0..ATTEMPTS {
            // SAFETY: the path is NUL terminated and lives through the call.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                pipe = Some(Pipe(handle));
                break;
            }
            // SAFETY: reads the thread-local error of the failed call above.
            last_error = unsafe { GetLastError() };
            match last_error {
                ERROR_PIPE_BUSY => {
                    // SAFETY: the path is valid; waits for a free instance.
                    unsafe { WaitNamedPipeW(path.as_ptr(), 200) };
                }
                ERROR_FILE_NOT_FOUND if attempt + 1 < ATTEMPTS => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let Some(pipe) = pipe else {
            return if matches!(last_error, ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY) {
                Err(HandoffError::NotRunning)
            } else {
                Err(std::io::Error::from_raw_os_error(last_error as i32).into())
            };
        };
        let mut frame = Vec::with_capacity(payload.len() + 4);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        write_all(pipe.0, &frame)?;
        // SAFETY: the handle is live; ensures the primary reads the frame even
        // if this process exits immediately afterwards.
        unsafe { FlushFileBuffers(pipe.0) };
        Ok(())
    }
}

#[cfg(unix)]
mod platform {
    use super::{HandoffError, MAX_HANDOFF_BYTES};
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A Unix socket address is capped near 104 bytes (`sun_path`), so a deep
    /// data root fails to bind with `SUN_LEN`. Such a path falls back to the
    /// short temp directory under a hash of itself: one data root still maps to
    /// one socket, and `bind` keeps the file at mode 0600 there as well.
    const SOCKET_PATH_LIMIT: usize = 96;

    pub(super) fn socket_path_in(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}.sock"));
        let encoded = path.as_os_str().as_encoded_bytes();
        if encoded.len() <= SOCKET_PATH_LIMIT {
            return path;
        }
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in encoded {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        std::env::temp_dir().join(format!("kaelis-{hash:016x}.sock"))
    }

    fn socket_path(name: &str) -> PathBuf {
        let dir = std::env::var_os("KAELIS_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                directories::ProjectDirs::from("gg", "kaelis", "Kaelis")
                    .map(|dirs| dirs.data_dir().to_path_buf())
            })
            .unwrap_or_else(std::env::temp_dir);
        socket_path_in(&dir, name)
    }

    pub(super) fn bind(name: &str) -> Result<super::HandoffServer, HandoffError> {
        let path = socket_path(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if path.exists() {
            // A live listener means another instance owns the guard; a stale
            // socket is removed so a crash does not block the next launch.
            match UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "another Kaelis instance owns the handoff socket",
                    )
                    .into());
                }
                Err(_) => std::fs::remove_file(&path)?,
            }
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("kaelis-handoff".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut header = [0_u8; 4];
                    if stream.read_exact(&mut header).is_err() {
                        continue;
                    }
                    let length = u32::from_be_bytes(header) as usize;
                    if length > MAX_HANDOFF_BYTES {
                        continue;
                    }
                    let mut payload = vec![0_u8; length];
                    if stream.read_exact(&mut payload).is_err() {
                        continue;
                    }
                    if let Ok(arguments) = serde_json::from_slice(&payload)
                        && sender.send(arguments).is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(super::HandoffServer {
            name: name.to_owned(),
            receiver,
        })
    }

    pub(super) fn send(name: &str, payload: &[u8]) -> Result<(), HandoffError> {
        let path = socket_path(name);
        // Same cold-start window as Windows: the socket appears a moment after
        // the guard, so a second launch retries instead of giving up.
        let mut stream = None;
        let mut last = None;
        for attempt in 0..40 {
            match UnixStream::connect(&path) {
                Ok(stream) => {
                    stream = Some(stream);
                    break;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionReset
                    ) =>
                {
                    last = Some(error);
                    if attempt < 39 {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        let Some(mut stream) = stream else {
            let _ = last;
            return Err(HandoffError::NotRunning);
        };
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut frame = Vec::with_capacity(payload.len() + 4);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        stream.write_all(&frame)?;
        stream.flush()?;
        Ok(())
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use super::HandoffError;

    pub(super) fn bind(_name: &str) -> Result<super::HandoffServer, HandoffError> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "single-instance handoff is not implemented for this target",
        )
        .into())
    }

    pub(super) fn send(_name: &str, _payload: &[u8]) -> Result<(), HandoffError> {
        Err(HandoffError::NotRunning)
    }
}

#[cfg(test)]
mod tests {
    use super::{HandoffError, HandoffServer, MAX_HANDOFF_BYTES, forward_to};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn unique_name(tag: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        format!("kaelis-test-{tag}-{}-{nanos}", std::process::id())
    }

    #[test]
    fn forwards_arguments_in_order() {
        let name = unique_name("order");
        let server = HandoffServer::bind_named(&name).expect("bind handoff server");
        let arguments = vec![
            "--play".to_owned(),
            "fabulous-1-21".to_owned(),
            "--offline".to_owned(),
            "Alex".to_owned(),
        ];
        forward_to(&name, &arguments).expect("forward arguments");
        assert_eq!(server.recv_timeout(Duration::from_secs(2)), Some(arguments));
    }

    #[test]
    fn forwards_empty_launch_as_activate_request() {
        let name = unique_name("empty");
        let server = HandoffServer::bind_named(&name).expect("bind handoff server");
        forward_to(&name, &[]).expect("forward empty launch");
        assert_eq!(
            server.recv_timeout(Duration::from_secs(2)),
            Some(Vec::new())
        );
    }

    /// macOS homes and temp roots are long enough that a nested data root blows
    /// past `sun_path`; the fallback must stay short and stay per-root.
    #[cfg(unix)]
    #[test]
    fn keeps_the_unix_socket_under_the_address_limit() {
        let deep = std::path::PathBuf::from("/tmp").join("data-root-".repeat(20));
        let fallback = super::platform::socket_path_in(&deep, "kaelis");
        assert!(fallback.as_os_str().as_encoded_bytes().len() <= 96);
        assert_ne!(
            super::platform::socket_path_in(&deep, "a"),
            super::platform::socket_path_in(&deep, "b")
        );
        assert_ne!(
            super::platform::socket_path_in(&deep, "kaelis"),
            super::platform::socket_path_in(&std::path::PathBuf::from("/tmp/other"), "kaelis")
        );
        let short = std::path::PathBuf::from("/tmp/kaelis");
        assert_eq!(
            super::platform::socket_path_in(&short, "kaelis"),
            short.join("kaelis.sock")
        );
    }

    #[test]
    fn rejects_payloads_over_the_bound() {
        let name = unique_name("oversize");
        let arguments = vec!["x".repeat(MAX_HANDOFF_BYTES)];
        assert!(matches!(
            forward_to(&name, &arguments),
            Err(HandoffError::TooLarge(_))
        ));
    }

    #[test]
    fn reports_a_missing_primary() {
        let name = unique_name("missing");
        assert!(matches!(
            forward_to(&name, &[]),
            Err(HandoffError::NotRunning)
        ));
    }
}
