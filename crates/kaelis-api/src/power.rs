//! Game process priority, scheduler latency, and power throttling.
//!
//! A Minecraft JVM competes with the browser, the launcher, and every other
//! process on the desktop. Windows exposes three levers that change that race
//! without touching the game itself:
//!
//! * **Priority class.** Every JVM Kaelis starts is promoted immediately after
//!   spawn and stays promoted for its whole life, so mod loading, chunk
//!   generation, and rendering are scheduled ahead of ordinary work even when
//!   the game is not the foreground window. `REALTIME_PRIORITY_CLASS` is tried
//!   first because it is the lowest-latency class Windows offers; it needs
//!   `SeIncreaseBasePriorityPrivilege`, so a normal (non-elevated) desktop
//!   refuses it and the launch silently settles on `HIGH_PRIORITY_CLASS`
//!   instead. Either way the game is never left at normal priority.
//! * **EcoQoS.** Windows may park a background process on efficiency cores at
//!   a reduced clock even at high priority. A launched game is explicitly
//!   opted out of execution-speed throttling, so a backgrounded or full-screen
//!   game keeps the performance cores for as long as it runs.
//! * **Timer resolution.** The scheduler's default 15.6 ms tick is far coarser
//!   than a frame. [`lock_timer_resolution`] holds a 0.5 ms tick for as long as
//!   a game is alive and releases it on every exit path, which flattens input,
//!   audio, and frame pacing without leaving a system-wide setting behind.
//!
//! Unix has no cross-process equivalent: an unprivileged process cannot raise
//! another process's scheduling priority, so this module compiles to a no-op
//! there. Lowering the game's own nice value would require elevation, and the
//! launcher does not ask for it.

/// The class a promoted process actually received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityClass {
    /// `REALTIME_PRIORITY_CLASS`: granted to an elevated host.
    Realtime,
    /// `HIGH_PRIORITY_CLASS`: the highest class a normal desktop allows.
    High,
    /// The host refused both; the game runs at its normal class.
    Unchanged,
}

/// Raise a freshly spawned JVM and opt it out of power throttling.
///
/// One call per launch: the class survives until the process exits, so there is
/// no monitor thread, no hook, and no timer to wake for a focus change. A pid
/// that has already exited, or a process the launcher may not touch, is a
/// silent no-op — the game runs normally either way.
pub fn promote(pid: u32) -> PriorityClass {
    #[cfg(windows)]
    {
        imp::promote(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        PriorityClass::Unchanged
    }
}

/// Hold a 0.5 ms scheduler tick until the guard is dropped.
///
/// The JVM inherits nothing from this call, so the guard is taken for the whole
/// life of a launch rather than for one request. Dropping it restores the
/// previous resolution and the previous power period, which is what keeps a
/// closed game from leaving a fast timer behind on the machine.
///
/// On Windows 10 2004 and newer the request is scoped to the calling process,
/// so the launcher's own UI and input loop get the finer tick while a game
/// runs, and the system timer is untouched. Older builds apply it globally for
/// as long as the launcher lives — also the intended behaviour.
pub fn lock_timer_resolution() -> LatencyGuard {
    #[cfg(windows)]
    {
        imp::lock_timer()
    }
    #[cfg(not(windows))]
    {
        LatencyGuard::inactive()
    }
}

/// RAII handle for the timer-resolution request. Inert off Windows and when
/// every API refuses the request.
pub struct LatencyGuard {
    #[cfg(windows)]
    state: imp::TimerState,
    #[cfg(not(windows))]
    _private: (),
}

impl LatencyGuard {
    #[cfg(not(windows))]
    const fn inactive() -> Self {
        Self { _private: () }
    }

    /// Whether a finer tick was actually granted.
    pub fn active(&self) -> bool {
        #[cfg(windows)]
        {
            self.state.half_ms || self.state.millisecond
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{LatencyGuard, PriorityClass};
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Threading::{
        HIGH_PRIORITY_CLASS, OpenProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, ProcessPowerThrottling,
        REALTIME_PRIORITY_CLASS, SetPriorityClass, SetProcessInformation,
    };

    /// `NtSetTimerResolution` in 100 ns units. 5,000 is exactly 0.5 ms; Windows
    /// clamps the request to the platform's own minimum.
    const HALF_MILLISECOND: u32 = 5_000;

    pub(super) fn promote(pid: u32) -> PriorityClass {
        if pid == 0 {
            return PriorityClass::Unchanged;
        }
        // SAFETY: no pointers; the pid comes from a child this launcher just
        // spawned. A process that has already exited makes OpenProcess fail,
        // which is handled below.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return PriorityClass::Unchanged;
        }
        // SAFETY: `handle` is a live process handle with SET_INFORMATION
        // access. Realtime needs a privilege an ordinary desktop does not
        // hold, so the first refusal is expected and the second call is the
        // real one.
        let class = unsafe {
            if SetPriorityClass(handle, REALTIME_PRIORITY_CLASS) != 0 {
                PriorityClass::Realtime
            } else if SetPriorityClass(handle, HIGH_PRIORITY_CLASS) != 0 {
                PriorityClass::High
            } else {
                PriorityClass::Unchanged
            }
        };
        // Opt the game out of EcoQoS. Without this Windows may schedule a
        // backgrounded JVM on efficiency cores at a reduced clock; clearing
        // the execution-speed bit keeps it on the performance cores for the
        // whole session. Advisory: a host that refuses the call still gets the
        // priority class above.
        // SAFETY: the structure is a plain `#[repr(C)]` value for the
        // documented class and the size passed is its own size.
        unsafe {
            let throttle = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                StateMask: 0,
            };
            SetProcessInformation(
                handle,
                ProcessPowerThrottling,
                std::ptr::from_ref(&throttle).cast::<c_void>(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            );
        }
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(handle) };
        class
    }

    /// What the timer request managed to obtain.
    pub(super) struct TimerState {
        pub(super) half_ms: bool,
        pub(super) millisecond: bool,
        /// The resolution Windows actually reported, so the release call can
        /// name it instead of guessing.
        previous: u32,
    }

    pub(super) fn lock_timer() -> LatencyGuard {
        let mut previous = 0_u32;
        let half_ms = nt_set_timer_resolution()
            .map(|set| {
                // SAFETY: the function pointer was resolved from ntdll, the
                // out-parameter is valid, and a non-zero NTSTATUS is a refusal.
                unsafe { set(HALF_MILLISECOND, 1, &mut previous) == 0 }
            })
            .unwrap_or(false);
        if half_ms {
            return LatencyGuard {
                state: TimerState {
                    half_ms: true,
                    millisecond: false,
                    previous,
                },
            };
        }
        // SAFETY: `timeBeginPeriod` only takes a scalar; a non-zero result is a
        // refusal and leaves the system at its default tick.
        let millisecond = unsafe { timeBeginPeriod(1) == 0 };
        LatencyGuard {
            state: TimerState {
                half_ms: false,
                millisecond,
                previous,
            },
        }
    }

    impl Drop for LatencyGuard {
        fn drop(&mut self) {
            if self.state.half_ms {
                if let Some(set) = nt_set_timer_resolution() {
                    let mut current = 0_u32;
                    // SAFETY: releasing the request this process holds; the
                    // out-parameter is valid and the status is advisory.
                    unsafe { set(self.state.previous, 0, &mut current) };
                }
            }
            if self.state.millisecond {
                // SAFETY: reverses the exact period requested above.
                unsafe { timeEndPeriod(1) };
            }
        }
    }

    type NtSetTimerResolution = unsafe extern "system" fn(u32, u8, *mut u32) -> i32;

    /// Resolve `NtSetTimerResolution` once. It is not in the Windows SDK's
    /// public import library, so the export is read from the loaded module
    /// instead of linked; a missing symbol falls back to `timeBeginPeriod`.
    fn nt_set_timer_resolution() -> Option<NtSetTimerResolution> {
        static RESOLVED: std::sync::OnceLock<Option<NtSetTimerResolution>> =
            std::sync::OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
            // SAFETY: the module name is NUL-terminated and both handles are
            // only read. `transmute` reinterprets the exported address as the
            // documented signature before any call.
            unsafe {
                let module = GetModuleHandleW(name.as_ptr());
                if module.is_null() {
                    return None;
                }
                let symbol = GetProcAddress(module, c"NtSetTimerResolution".as_ptr().cast());
                symbol.map(|raw| {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, NtSetTimerResolution>(
                        raw,
                    )
                })
            }
        })
    }

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn timeBeginPeriod(period: u32) -> u32;
        fn timeEndPeriod(period: u32) -> u32;
    }

    #[cfg(test)]
    mod tests {
        use super::{lock_timer, promote};
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetPriorityClass, HIGH_PRIORITY_CLASS, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            REALTIME_PRIORITY_CLASS,
        };

        fn priority(pid: u32) -> Option<u32> {
            // SAFETY: the pid belongs to the live child below, and the handle
            // is queried with the least access that can answer the question.
            unsafe {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if handle.is_null() {
                    return None;
                }
                let class = GetPriorityClass(handle);
                CloseHandle(handle);
                (class != 0).then_some(class)
            }
        }

        #[test]
        fn a_launched_game_is_promoted() {
            let mut child = std::process::Command::new("cmd")
                .args(["/C", "ping -n 8 127.0.0.1 >NUL"])
                .spawn()
                .expect("a child process that outlives the promotion");
            let pid = child.id();
            promote(pid);
            let class = priority(pid);
            assert!(
                matches!(
                    class,
                    Some(REALTIME_PRIORITY_CLASS) | Some(HIGH_PRIORITY_CLASS)
                ),
                "expected realtime or high, got {class:?}"
            );
            let _ = child.kill();
            let _ = child.wait();
        }

        #[test]
        fn a_timer_lock_can_be_taken_and_released() {
            let guard = lock_timer();
            let _active = guard.active();
            drop(guard);
            // A second request after the first was released must not error.
            drop(lock_timer());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PriorityClass, lock_timer_resolution, promote};

    #[test]
    fn promoting_an_impossible_pid_is_silent() {
        let class = promote(0);
        assert!(matches!(
            class,
            PriorityClass::Realtime | PriorityClass::High | PriorityClass::Unchanged
        ));
    }

    #[test]
    fn the_latency_guard_is_always_droppable() {
        let guard = lock_timer_resolution();
        let _ = guard.active();
        drop(guard);
    }
}
