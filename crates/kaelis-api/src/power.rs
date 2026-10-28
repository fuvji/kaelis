//! Game process priority.
//!
//! A Minecraft JVM competes with the browser, the launcher, and every other
//! process on the desktop. Windows exposes one safe lever for that: the process
//! priority class. Every JVM Kaelis starts is raised to `HIGH_PRIORITY_CLASS`
//! immediately after spawn and stays there for its whole life, so mod loading,
//! chunk generation, and rendering are scheduled ahead of ordinary background
//! work even when the game is not the foreground window.
//!
//! `REALTIME_PRIORITY_CLASS` is deliberately not used. It needs
//! `SeIncreaseBasePriorityPrivilege`, so it only succeeds from an elevated
//! launcher, and a realtime JVM can starve the input, audio, and disk threads it
//! depends on. High is the highest class that is both reachable and safe, and it
//! is the ceiling used here.
//!
//! Unix has no cross-process equivalent: an unprivileged process cannot raise
//! another process's scheduling priority, so this module compiles to a no-op
//! there. Lowering the game's own nice value would require elevation, and the
//! launcher does not ask for it.

/// Raise a freshly spawned JVM to the high priority class.
///
/// One call per launch: the class survives until the process exits, so there is
/// no monitor thread, no hook, and no timer to wake for a focus change. A pid
/// that has already exited, or a process the launcher may not touch, is a silent
/// no-op — the game runs normally either way.
pub fn promote(pid: u32) {
    #[cfg(windows)]
    imp::promote(pid);
    #[cfg(not(windows))]
    let _ = pid;
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        HIGH_PRIORITY_CLASS, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SET_INFORMATION, SetPriorityClass,
    };

    pub(super) fn promote(pid: u32) {
        if pid == 0 {
            return;
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
            return;
        }
        // SAFETY: `handle` is a live process handle with SET_INFORMATION
        // access, and HIGH is the highest class available without elevation.
        unsafe { SetPriorityClass(handle, HIGH_PRIORITY_CLASS) };
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(handle) };
    }

    #[cfg(test)]
    mod tests {
        use super::promote;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetPriorityClass, HIGH_PRIORITY_CLASS, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
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
        fn a_launched_game_stays_high() {
            let mut child = std::process::Command::new("cmd")
                .args(["/C", "ping -n 8 127.0.0.1 >NUL"])
                .spawn()
                .expect("a child process that outlives the promotion");
            let pid = child.id();
            promote(pid);
            assert_eq!(priority(pid), Some(HIGH_PRIORITY_CLASS));
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
