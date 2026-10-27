//! Game processes Kaelis started, remembered across launcher restarts.
//!
//! The in-memory running set dies with the window. Quitting Kaelis while
//! Minecraft keeps playing is supported, so a fresh launcher used to show a
//! green Play button for a game that was still running — and clicking it
//! started a second JVM on the same world.
//!
//! A launch writes one tiny record next to the instance data: the process id
//! and its creation time. On the next start every record is checked against
//! the live process table. The creation time is what makes this safe: Windows
//! reuses process ids, but a reused id never shares the creation timestamp of
//! the dead game. Stale records are removed instead of being trusted.
//!
//! Nothing here scans every process on the machine. A launch costs one small
//! file write, recovery costs one file read per game, and validating a record
//! costs one handle open. That is why a launcher with a dozen instances still
//! opens in milliseconds.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One remembered game process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningGame {
    pub pid: u32,
    /// Process creation time in 100 ns ticks since 1601 on Windows; `0` when
    /// the platform cannot report it, in which case only the pid is checked.
    #[serde(default)]
    pub started: u64,
}

impl RunningGame {
    pub fn is_valid(&self) -> bool {
        if self.pid == 0 {
            return false;
        }
        validate(self.pid, self.started)
    }
}

/// Removes a game's record when the launch worker ends, however it ends.
///
/// Dropping the guard on cancellation, an error return, or an ordinary exit
/// keeps the registry honest without a cleanup thread.
pub struct RunningMark {
    instance_id: String,
}

impl Drop for RunningMark {
    fn drop(&mut self) {
        clear(&self.instance_id);
    }
}

/// Publish a spawned game and return the guard that removes its record.
pub fn mark(instance_id: &str, pid: u32) -> RunningMark {
    let started = creation_time(pid).unwrap_or(0);
    let _ = write_record(instance_id, RunningGame { pid, started });
    RunningMark {
        instance_id: instance_id.to_owned(),
    }
}

/// Forget one instance, whether or not a record exists.
pub fn clear(instance_id: &str) {
    if let Some(path) = record_path(instance_id) {
        let _ = std::fs::remove_file(path);
    }
}

/// Every record whose process is still alive, with stale records removed.
pub fn live() -> Vec<(String, RunningGame)> {
    let directory = records_dir();
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut games = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(instance_id) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(game) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RunningGame>(&bytes).ok())
        else {
            // A half-written record from a launcher killed mid-save is not a
            // running game; remove it instead of leaving litter.
            let _ = std::fs::remove_file(&path);
            continue;
        };
        if game.is_valid() {
            games.push((instance_id, game));
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    games
}

/// The record for one instance, if its process is still alive.
pub fn find(instance_id: &str) -> Option<RunningGame> {
    live()
        .into_iter()
        .find(|(id, _)| id == instance_id)
        .map(|(_, game)| game)
}

/// End a remembered game process now. Used when Quit is pressed for a game
/// that was adopted after a launcher restart and therefore has no launch task
/// left to signal.
pub fn terminate(game: &RunningGame) -> bool {
    stop(game.pid, game.started)
}

/// Block until the process is gone. The caller runs this on a worker thread
/// and turns the return into one exit event.
pub fn wait_until_gone(game: &RunningGame) {
    wait(game.pid, game.started);
}

fn records_dir() -> PathBuf {
    let instances = crate::instance::Instance::instances_dir();
    instances
        .parent()
        .map(|root| root.join("running"))
        .unwrap_or_else(|| instances.join("running"))
}

fn record_path(instance_id: &str) -> Option<PathBuf> {
    let safe = sanitize(instance_id)?;
    Some(records_dir().join(format!("{safe}.json")))
}

fn write_record(instance_id: &str, game: RunningGame) -> std::io::Result<()> {
    let Some(path) = record_path(instance_id) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(&game)?;
    // Write beside the final name and rename, so a launcher killed halfway
    // through can never leave a record that parses as a running game.
    let staging = path.with_extension("json.part");
    std::fs::write(&staging, bytes)?;
    std::fs::rename(&staging, &path)?;
    Ok(())
}

/// Instance ids are folder slugs; keep the same alphabet so a record can never
/// escape the running directory.
fn sanitize(instance_id: &str) -> Option<String> {
    if instance_id.is_empty() || instance_id.len() > 96 {
        return None;
    }
    instance_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        .then(|| instance_id.to_owned())
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    fn ticks(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }

    pub(super) fn validate(pid: u32, started: u64) -> bool {
        // SAFETY: a pid and a rights mask only; the handle is checked and
        // closed below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: every out-pointer refers to a live local FILETIME.
        let ok =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
                != 0;
        // SAFETY: the handle came from OpenProcess and is owned here.
        unsafe { CloseHandle(handle) };
        ok && (started == 0 || ticks(creation) == started)
    }

    pub(super) fn creation_time(pid: u32) -> Option<u64> {
        // SAFETY: as above; the handle is closed before returning.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: every out-pointer refers to a live local FILETIME.
        let ok =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
                != 0;
        // SAFETY: the handle came from OpenProcess and is owned here.
        unsafe { CloseHandle(handle) };
        ok.then(|| ticks(creation))
    }

    pub(super) fn stop(pid: u32, started: u64) -> bool {
        // Refuse a recycled pid: the whole point of the timestamp.
        if !validate(pid, started) {
            return false;
        }
        // SAFETY: the handle is checked, used once, and closed.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: a valid process handle with PROCESS_TERMINATE; exit code 1
        // is the conventional forced termination.
        let ok = unsafe { TerminateProcess(handle, 1) } != 0;
        // SAFETY: the handle came from OpenProcess and is owned here.
        unsafe { CloseHandle(handle) };
        ok
    }

    pub(super) fn wait(pid: u32, started: u64) {
        // A missing process is already "gone".
        if !validate(pid, started) {
            return;
        }
        // SAFETY: request only SYNCHRONIZE; the handle is closed below.
        let handle: HANDLE = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return;
        }
        // SAFETY: a synchronize-capable process handle waits until it exits.
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            CloseHandle(handle);
        }
    }
}

#[cfg(windows)]
use imp::{creation_time, stop, validate, wait};

#[cfg(unix)]
mod imp {
    fn pid_fits(pid: u32) -> bool {
        pid != 0 && pid <= i32::MAX as u32
    }

    pub(super) fn validate(pid: u32, _started: u64) -> bool {
        if !pid_fits(pid) {
            return false;
        }
        // SAFETY: signal 0 performs the permission/existence check only.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    pub(super) fn creation_time(_pid: u32) -> Option<u64> {
        // Portable creation time is not available; the pid check is the
        // documented best effort on these hosts.
        None
    }

    pub(super) fn stop(pid: u32, started: u64) -> bool {
        if !validate(pid, started) {
            return false;
        }
        // SAFETY: the pid was validated as a live process we can signal.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) == 0 }
    }

    pub(super) fn wait(pid: u32, started: u64) {
        // One syscall every 100 ms, no /proc parsing. The shorter step is what
        // the user feels: it is the lag between quitting the game and the
        // launcher coming back, and ten checks a second costs nothing while a
        // game is running.
        while validate(pid, started) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(unix)]
use imp::{creation_time, stop, validate, wait};

#[cfg(not(any(windows, unix)))]
mod imp {
    pub(super) fn validate(_pid: u32, _started: u64) -> bool {
        false
    }
    pub(super) fn creation_time(_pid: u32) -> Option<u64> {
        None
    }
    pub(super) fn stop(_pid: u32, _started: u64) -> bool {
        false
    }
    pub(super) fn wait(_pid: u32, _started: u64) {}
}

#[cfg(not(any(windows, unix)))]
use imp::{creation_time, stop, validate, wait};

/// Directory holding the records. Exposed for tests and portable imports.
pub fn data_dir() -> PathBuf {
    records_dir()
}

/// Read a record without validating it, for diagnostics and tests.
pub fn record_for(instance_id: &str) -> Option<RunningGame> {
    let path = record_path(instance_id)?;
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether a path is one of our records, so the junk sweeper never touches it.
pub fn is_record_path(path: &Path) -> bool {
    path.starts_with(records_dir())
}

#[cfg(test)]
mod tests {
    use super::{RunningGame, clear, find, live, mark, record_for, terminate};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    /// `live()` legitimately prunes records from other ids, so tests that
    /// share one records directory must not interleave.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn probe_id() -> String {
        format!(
            "kaelis-running-probe-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn a_live_process_is_remembered_and_then_forgotten() {
        let _guard = lock();
        let id = probe_id();
        let guard = mark(&id, std::process::id());
        // The record exists and validates for this very process, which is the
        // same check a fresh launcher performs at start-up.
        assert!(record_for(&id).is_some());
        assert!(find(&id).is_some());
        drop(guard);
        assert!(record_for(&id).is_none());
        assert!(find(&id).is_none());
    }

    #[test]
    fn a_dead_pid_is_pruned_instead_of_reported() {
        let _guard = lock();
        let id = probe_id();
        // A pid that cannot belong to a live game on either platform.
        super::write_record(
            &id,
            RunningGame {
                pid: u32::MAX - 1,
                started: 0,
            },
        )
        .expect("record writes");
        assert!(super::record_for(&id).is_some());
        assert!(find(&id).is_none());
        assert!(!live().iter().any(|(probe, _)| probe == &id));
        assert!(super::record_for(&id).is_none());
        clear(&id);
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn a_recorded_child_can_be_ended() {
        let _guard = lock();
        let id = probe_id();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .expect("start a long-lived child");
        #[cfg(unix)]
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("start a long-lived child");

        let _mark = mark(&id, child.id());
        let game = record_for(&id).expect("record exists while the child runs");
        assert!(game.is_valid());
        assert!(terminate(&game), "the recorded process must be killable");
        let status = child.wait().expect("child exits after termination");
        assert!(!status.success());
        // The guard removes the record; nothing is left for a later launcher.
        drop(_mark);
        assert!(record_for(&id).is_none());
    }

    #[test]
    fn instance_ids_cannot_escape_the_record_directory() {
        let _guard = lock();
        assert!(super::record_path("../escape").is_none());
        assert!(super::record_path("a/b").is_none());
        assert!(super::record_path("").is_none());
        assert!(super::record_path("fine-id_1").is_some());
    }
}
