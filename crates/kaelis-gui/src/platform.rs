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
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, SW_RESTORE, SW_SHOW,
        SetForegroundWindow, ShowWindow,
    };

    struct Target {
        pid: u32,
        title: Vec<u16>,
        found: isize,
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
        if length <= 0 || buffer[..length as usize] != target.title {
            return 1;
        }
        target.found = window as isize;
        0
    }

    let title = kaelis_core::brand::LAUNCHER_NAME
        .encode_utf16()
        .collect::<Vec<_>>();
    let mut target = Target {
        // SAFETY: no arguments, always succeeds.
        pid: unsafe { GetCurrentProcessId() },
        title,
        found: 0,
    };
    // SAFETY: the callback only touches `target` through the pointer we pass,
    // and both live until EnumWindows returns.
    unsafe {
        EnumWindows(Some(find_window), &mut target as *mut Target as isize);
    }
    if target.found == 0 {
        return;
    }
    let window = target.found as windows_sys::Win32::Foundation::HWND;
    // SAFETY: `window` was returned by EnumWindows for this process.
    unsafe {
        ShowWindow(window, SW_RESTORE);
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
    }
}
