//! Native window handling that egui cannot do by itself.
//!
//! egui only applies `ViewportCommand`s while it is painting a frame, and a
//! hidden window may never paint again. Kaelis therefore keeps a platform
//! restore path: on Windows the real HWND is shown from the game-exit worker,
//! and elsewhere the window is minimised instead of hidden, so the toolkit can
//! always deliver the queued restore commands.

use eframe::egui;

/// Put the launcher to sleep while Minecraft runs.
///
/// Windows hides the window completely and restores it through Win32 when the
/// JVM exits. X11, Wayland, and macOS are minimised instead: an unmapped window
/// cannot be relied on to process the queued "become visible" command, and a
/// minimised window stays in the taskbar and Dock.
pub(crate) fn hide_for_game(context: &egui::Context) {
    context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    #[cfg(windows)]
    context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
}

/// Bring the launcher back the instant the game process is gone.
///
/// On Windows this restores the real window first, because a hidden viewport
/// paints no frames and would never apply egui's own command. Other platforms
/// only need the toolkit commands, which the caller sends right after.
pub(crate) fn restore_now() {
    #[cfg(windows)]
    restore_windows_window();
}

#[cfg(windows)]
fn restore_windows_window() {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, SW_RESTORE, SW_SHOW,
        SetForegroundWindow, ShowWindow,
    };

    struct Target {
        pid: u32,
        title: Vec<u16>,
        found: isize,
        /// Any titled, non-console top-level window of this process. The
        /// launcher title is the fast path; this keeps the window reachable if
        /// a future build ever renders a dynamic title.
        fallback: isize,
    }

    /// ASCII-only case fold for a UTF-16 code unit; non-letters pass through.
    fn fold(value: u16) -> u16 {
        if (u16::from(b'A')..=u16::from(b'Z')).contains(&value) {
            value + 32
        } else {
            value
        }
    }

    unsafe extern "system" fn find_window(
        window: windows_sys::Win32::Foundation::HWND,
        parameter: windows_sys::Win32::Foundation::LPARAM,
    ) -> windows_sys::Win32::Foundation::BOOL {
        // SAFETY: `parameter` is the `&mut Target` passed to EnumWindows below
        // and lives for the duration of the enumeration.
        let target = unsafe { &mut *(parameter as *mut Target) };
        let mut pid = 0_u32;
        // SAFETY: the handle comes from EnumWindows and the out-pointer is valid.
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        if pid != target.pid {
            return 1;
        }
        let mut buffer = [0_u16; 128];
        // SAFETY: the buffer is valid for its declared length.
        let length = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
        if length <= 0 {
            return 1;
        }
        let title = &buffer[..length as usize];
        // Prefix, not equality: the launcher may append the instance name, and
        // a hidden window must still be found after any title change.
        let matches = title.len() >= target.title.len()
            && title
                .iter()
                .zip(&target.title)
                .all(|(left, right)| fold(*left) == fold(*right));
        if matches {
            target.found = window as isize;
            return 0;
        }
        if target.fallback == 0 {
            let mut class = [0_u16; 64];
            // SAFETY: the buffer is valid for its declared length.
            let class_length =
                unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
            let console = class_length > 0 && {
                let name = String::from_utf16_lossy(&class[..class_length as usize]);
                name.eq_ignore_ascii_case("ConsoleWindowClass")
            };
            if !console {
                target.fallback = window as isize;
            }
        }
        1
    }

    let title = kaelis_core::brand::LAUNCHER_NAME
        .encode_utf16()
        .collect::<Vec<_>>();
    let mut target = Target {
        // SAFETY: no arguments, always succeeds.
        pid: unsafe { GetCurrentProcessId() },
        title,
        found: 0,
        fallback: 0,
    };
    // SAFETY: the callback only touches `target` through the pointer we pass,
    // and both live until EnumWindows returns.
    unsafe {
        EnumWindows(Some(find_window), &mut target as *mut Target as isize);
    }
    let found = if target.found != 0 {
        target.found
    } else {
        target.fallback
    };
    if found == 0 {
        return;
    }
    let window = found as windows_sys::Win32::Foundation::HWND;
    // SAFETY: `window` was returned by EnumWindows for this process.
    unsafe {
        ShowWindow(window, SW_RESTORE);
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
    }
}
