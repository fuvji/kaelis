//! Background-instance power governor.
//!
//! Several Minecraft instances can run at once. The one whose window owns the
//! foreground deserves the CPU; the others are idling behind it while still
//! being scheduled at the same priority, which is exactly how a second
//! instance makes the active one stutter. Each JVM Kaelis starts is registered
//! here, and a small monitor demotes every unfocused one to the idle priority
//! class and restores normal priority the moment its window is focused again.
//!
//! This is a scheduling decision only: no process is suspended, no session is
//! disconnected, and Minecraft keeps ticking normally in the background. It
//! simply stops competing with the game the user is actually playing.

use std::sync::{Mutex, OnceLock};

struct Tracked {
    pid: u32,
    demoted: bool,
}

static GAMES: OnceLock<Mutex<Vec<Tracked>>> = OnceLock::new();

/// Removes one JVM from the governor when its launch worker ends.
pub struct LiveGame {
    pid: u32,
}

impl Drop for LiveGame {
    fn drop(&mut self) {
        if let Some(games) = GAMES.get()
            && let Ok(mut games) = games.lock()
        {
            games.retain(|game| game.pid != self.pid);
        }
    }
}

/// Register a freshly spawned JVM. Cheap on every platform; only Windows has
/// a priority class to steer, and the idle platforms compile to just the guard.
pub fn track(pid: u32) -> LiveGame {
    let games = GAMES.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut games) = games.lock()
        && !games.iter().any(|game| game.pid == pid)
    {
        games.push(Tracked {
            pid,
            demoted: false,
        });
    }
    #[cfg(windows)]
    imp::ensure_monitor();
    LiveGame { pid }
}

#[cfg(windows)]
mod imp {
    use super::GAMES;
    use std::sync::Once;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SET_INFORMATION, SetPriorityClass,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    static STARTED: Once = Once::new();

    pub(super) fn ensure_monitor() {
        STARTED.call_once(|| {
            // A named thread so a debugger or a process dump names the work.
            // It wakes every 1.5 s while a game runs and does one foreground
            // lookup plus at most one priority call per instance.
            let _ = std::thread::Builder::new()
                .name("kaelis-power-governor".to_owned())
                .spawn(|| {
                    loop {
                        std::thread::sleep(Duration::from_millis(1500));
                        let Some(games) = GAMES.get() else {
                            continue;
                        };
                        let Ok(mut games) = games.lock() else {
                            continue;
                        };
                        if games.is_empty() {
                            continue;
                        }
                        let foreground = foreground_pid();
                        for game in games.iter_mut() {
                            let idle = Some(game.pid) != foreground;
                            if idle != game.demoted && set_priority(game.pid, idle) {
                                game.demoted = idle;
                            }
                        }
                    }
                });
        });
    }

    fn foreground_pid() -> Option<u32> {
        let mut pid = 0_u32;
        // SAFETY: no arguments; the out-pointer is valid. A missing foreground
        // window (session lock) simply reports pid 0, which demotes every game.
        let window = unsafe { GetForegroundWindow() };
        if window.is_null() {
            return None;
        }
        // SAFETY: the handle is a live top-level window and the pointer is valid.
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        (pid != 0).then_some(pid)
    }

    fn set_priority(pid: u32, idle: bool) -> bool {
        // SAFETY: no pointers; the process id comes from a child this launcher
        // owns. A dead process makes OpenProcess fail, which is handled.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return false;
        }
        // SAFETY: `handle` is a process handle with SET_INFORMATION access.
        let applied = unsafe {
            SetPriorityClass(
                handle,
                if idle {
                    IDLE_PRIORITY_CLASS
                } else {
                    NORMAL_PRIORITY_CLASS
                },
            )
        };
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(handle) };
        applied != 0
    }
}
