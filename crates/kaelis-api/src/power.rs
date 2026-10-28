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
//! simply stops competing with the game the user is actually playing. A
//! demoted instance also gives up the fast cores through EcoQoS, so an
//! alt-tabbed world stops drawing power and heating the machine while the
//! instance in front keeps the whole budget.

use std::sync::{Mutex, OnceLock};

struct Tracked {
    pid: u32,
    /// Only the Windows monitor reads this. Keeping the field out of other
    /// builds avoids a dead-code warning under `-D warnings` on Unix, where
    /// the governor has no priority class to steer.
    #[cfg(windows)]
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
            #[cfg(windows)]
            demoted: false,
        });
    }
    #[cfg(windows)]
    {
        // The hook is installed once and costs nothing between events, so a
        // launch never waits for a timer to notice it.
        imp::ensure_monitor();
        // A second instance started while a game owns the screen is demoted
        // right here, without waiting for the next foreground change: the lock
        // above is released before this call, which takes it again.
        imp::apply();
    }
    LiveGame { pid }
}

#[cfg(windows)]
mod imp {
    use super::GAMES;
    use std::sync::Once;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND};
    use windows_sys::Win32::System::Threading::{
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, OpenProcess,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
        ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
    };
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, MSG,
        WINEVENT_OUTOFCONTEXT,
    };

    static STARTED: Once = Once::new();

    pub(super) fn ensure_monitor() {
        STARTED.call_once(|| {
            // A named thread so a debugger or a process dump names the work.
            // It blocks in the message loop below and wakes only when the
            // foreground window actually changes, so an idle launcher with a
            // game running costs no timer, no wake-up, and no polling at all —
            // and the demote happens the instant the user switches away
            // instead of up to a second later.
            let _ = std::thread::Builder::new()
                .name("kaelis-power-governor".to_owned())
                .spawn(|| {
                    /// One foreground change: re-steer every tracked instance.
                    unsafe extern "system" fn foreground_changed(
                        _hook: HWINEVENTHOOK,
                        _event: u32,
                        _window: HWND,
                        _object: i32,
                        _child: i32,
                        _thread: u32,
                        _time: u32,
                    ) {
                        apply();
                    }

                    // SAFETY: the hook is process-wide, the callback is a plain
                    // `fn` that only reads the tracked list, no module handle is
                    // needed for an out-of-context hook, and a null result is
                    // checked.
                    let hook = unsafe {
                        SetWinEventHook(
                            EVENT_SYSTEM_FOREGROUND,
                            EVENT_SYSTEM_FOREGROUND,
                            std::ptr::null_mut(),
                            Some(foreground_changed),
                            0,
                            0,
                            WINEVENT_OUTOFCONTEXT,
                        )
                    };
                    if hook.is_null() {
                        // A host that refuses the hook still gets the steering
                        // at registration time, just not on focus changes.
                        return;
                    }
                    let mut message: MSG = unsafe { std::mem::zeroed() };
                    // Out-of-context events arrive through this thread's queue,
                    // so it must pump messages. The loop ends only with the
                    // process.
                    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {}
                });
        });
    }

    /// Steer every tracked JVM: the one in front runs normally, the rest idle.
    pub(super) fn apply() {
        let Some(games) = GAMES.get() else {
            return;
        };
        let Ok(mut games) = games.lock() else {
            return;
        };
        if games.is_empty() {
            return;
        }
        let foreground = foreground_pid();
        for game in games.iter_mut() {
            let idle = Some(game.pid) != foreground;
            if idle != game.demoted && set_priority(game.pid, idle) {
                game.demoted = idle;
            }
        }
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
        // EcoQoS is the power half of the same decision: an instance nobody is
        // looking at is allowed to run on the efficiency cores at a low clock,
        // so it cannot take the fast cores or the power budget from the world
        // the user is playing. It is advisory — a host that ignores it keeps
        // the priority class above.
        let throttle = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: if idle {
                PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            } else {
                0
            },
        };
        // SAFETY: the handle allows SET_INFORMATION, the structure is a plain
        // `#[repr(C)]` value for the documented class, and the size passed is
        // its own size.
        unsafe {
            SetProcessInformation(
                handle,
                ProcessPowerThrottling,
                std::ptr::from_ref(&throttle).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            );
        }
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(handle) };
        applied != 0
    }
}
