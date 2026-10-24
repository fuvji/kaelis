//! Native window handling that egui cannot do by itself.
//!
//! egui applies `ViewportCommand`s only while it is painting a frame, and a
//! hidden window may never paint again. Kaelis therefore never asks egui to
//! hide its viewport: on Windows the real HWND is hidden with Win32 and shown
//! with Win32, so the toolkit's idea of the window never goes stale. Every
//! restore also raises the window above a running game and asks for one paint,
//! which is what makes the tray item work while Minecraft owns the screen.

use eframe::egui;

/// Put the launcher to sleep while Minecraft runs.
///
/// Windows hides the real window without touching egui's viewport state: a
/// `ViewportCommand::Visible(false)` leaves the toolkit convinced the window
/// is invisible, so a later native `ShowWindow` produces an unpainted, blank
/// frame. X11, Wayland, and macOS are minimised instead, because an unmapped
/// window there cannot be relied on to process the queued restore.
pub(crate) fn hide_for_game(context: &egui::Context) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        let _ = context;
        if let Some(window) = main_window() {
            // SAFETY: `window` was returned by EnumWindows for this process.
            unsafe {
                ShowWindow(window, SW_HIDE);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = context;
        context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

/// Give back the pages the hidden launcher no longer needs.
///
/// Hiding a window stops it painting, but its working set still holds the
/// textures, fonts, and decoded artwork from the last frame. An offline game
/// session is exactly the moment none of that is visible, so the OS is asked
/// to trim the process to its resident minimum; the pages fault back in when
/// the window is restored.
pub(crate) fn trim_working_set() {
    #[cfg(windows)]
    {
        // The caller is inside a paint frame. Trimming immediately would be
        // undone by the rest of that frame, which touches the same fonts and
        // textures again before it finishes. Give the frame 200 ms to end,
        // then release the pages with no window update left to fault them
        // straight back in. A fresh launcher wakes only intermittently, so one
        // short-lived thread per trim is cheaper than a resident timer.
        let spawned = std::thread::Builder::new()
            .name("kaelis-hibernate".to_owned())
            .spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                release_pages();
            });
        if spawned.is_err() {
            release_pages();
        }
    }
}

#[cfg(windows)]
fn release_pages() {
    use windows_sys::Win32::System::ProcessStatus::EmptyWorkingSet;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: the pseudo-handle is always valid and refers to this process;
    // trimming is advisory and its result needs no handling.
    unsafe {
        EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Start the event-driven idle guard.
///
/// A `WaitForSingleObject`-style timer would have to poll, and egui cannot
/// guarantee a frame after focus loss (an idle window schedules none, which is
/// the whole point). Instead a Win32 foreground hook wakes this thread only
/// when the foreground window actually changes. One short-lived sleeper is
/// armed for the first transition away from Kaelis; regaining focus bumps a
/// generation counter so that sleeper exits without touching a live window.
/// After the trim there is nothing left to schedule until the next focus loss.
#[cfg(windows)]
pub(crate) fn start_idle_guard() {
    use std::sync::Once;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EVENT_SYSTEM_FOREGROUND, GetMessageW, MSG, WINEVENT_OUTOFCONTEXT,
    };
    static STARTED: Once = Once::new();
    static GENERATION: AtomicU64 = AtomicU64::new(0);
    static UNFOCUSED: AtomicBool = AtomicBool::new(false);

    fn foreground_is_ours() -> bool {
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        // SAFETY: both calls take no pointers beyond a valid out-parameter.
        unsafe {
            let window = GetForegroundWindow();
            if window.is_null() {
                return false;
            }
            let mut pid = 0_u32;
            GetWindowThreadProcessId(window, &mut pid);
            pid == GetCurrentProcessId()
        }
    }

    unsafe extern "system" fn foreground_changed(
        _hook: HWINEVENTHOOK,
        _event: u32,
        _window: windows_sys::Win32::Foundation::HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if foreground_is_ours() {
            UNFOCUSED.store(false, std::sync::atomic::Ordering::Relaxed);
            // Invalidate any sleeper armed by the focus loss before this one.
            GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        } else if !UNFOCUSED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let _ = std::thread::Builder::new()
                .name("kaelis-hibernate-wait".to_owned())
                .spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(90));
                    if GENERATION.load(std::sync::atomic::Ordering::Relaxed) == generation
                        && !foreground_is_ours()
                    {
                        release_pages();
                    }
                });
        }
    }

    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("kaelis-idle-guard".to_owned())
            .spawn(|| {
                // SAFETY: the hook is process-wide, the callback is a plain fn,
                // and the pointer is checked before the message loop starts.
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
                    return;
                }
                let mut message: MSG = unsafe { std::mem::zeroed() };
                // Out-of-context events arrive through this thread's queue, so
                // it must pump messages. The loop ends only with the process.
                while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {}
            });
    });
}

#[cfg(not(windows))]
pub(crate) fn start_idle_guard() {}

/// Show, raise, and repaint the launcher from a background thread.
///
/// This is the one path every restore uses: the tray menu, the tray icon, and
/// a second launcher process. The native show happens first because a hidden
/// window paints nothing; the viewport commands then keep egui's state in
/// agreement, and the final repaint produces a frame immediately instead of
/// waiting for the next input event.
pub(crate) fn wake(context: &egui::Context) {
    #[cfg(windows)]
    restore_windows_window();
    context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    context.send_viewport_cmd(egui::ViewportCommand::Focus);
    context.request_repaint();
}

/// Bring the launcher back the instant the game process is gone.
///
/// Kept separate from [`wake`] for callers that have no egui context (the game
/// exit worker restores the real window, and the next frame paints it).
pub(crate) fn restore_now() {
    #[cfg(windows)]
    restore_windows_window();
}

#[cfg(windows)]
fn main_window() -> Option<windows_sys::Win32::Foundation::HWND> {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
    };

    struct Target {
        pid: u32,
        expected: Vec<u16>,
        /// The launcher window: title starts with the brand.
        found: isize,
        /// Any titled, non-console window of this process. Used only when the
        /// title does not match, so a renamed build stays reachable.
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
        let mut title = [0_u16; 256];
        // SAFETY: the buffer is valid for its declared length.
        let title_length =
            unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
        if title_length <= 0 {
            return 1;
        }
        let title = &title[..title_length as usize];
        let mut class = [0_u16; 64];
        // SAFETY: the buffer is valid for its declared length.
        let class_length = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        let console = class_length > 0 && {
            let name = String::from_utf16_lossy(&class[..class_length as usize]);
            name.eq_ignore_ascii_case("ConsoleWindowClass")
        };
        if !console && target.fallback == 0 {
            target.fallback = window as isize;
        }
        // Prefix, not equality: the launcher may append an instance name.
        if title.len() >= target.expected.len()
            && title
                .iter()
                .zip(&target.expected)
                .all(|(left, right)| fold(*left) == fold(*right))
        {
            target.found = window as isize;
            return 0;
        }
        1
    }

    let mut target = Target {
        // SAFETY: no arguments, always succeeds.
        pid: unsafe { GetCurrentProcessId() },
        expected: kaelis_core::brand::LAUNCHER_NAME.encode_utf16().collect(),
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
    (found != 0).then_some(found as windows_sys::Win32::Foundation::HWND)
}

#[cfg(windows)]
fn restore_windows_window() {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, SW_RESTORE,
        SW_SHOW, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos,
        ShowWindow, SwitchToThisWindow,
    };

    let Some(window) = main_window() else {
        return;
    };
    // SAFETY: `window` belongs to this process; every call below accepts a
    // top-level window and only affects its visibility and z-order.
    unsafe {
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        ShowWindow(window, SW_SHOW);
        // A plain SetForegroundWindow is refused while another application
        // owns the foreground — exactly the case of a fullscreen game. The
        // topmost round-trip plus thread attachment is the pair Windows
        // honours from a background process.
        SetWindowPos(
            window,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
        SetWindowPos(
            window,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
        BringWindowToTop(window);
        let foreground = GetForegroundWindow();
        let mut foreground_pid = 0_u32;
        let foreground_thread =
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                foreground,
                &mut foreground_pid,
            );
        let current_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0
            && foreground_thread != current_thread
            && AttachThreadInput(current_thread, foreground_thread, 1) != 0;
        SetForegroundWindow(window);
        if attached {
            AttachThreadInput(current_thread, foreground_thread, 0);
        }
        // Windows can still decline focus to an application the user did not
        // click. SwitchToThisWindow is the documented-by-behaviour fallback
        // that makes the window at least appear above the game.
        if GetForegroundWindow() != window {
            SwitchToThisWindow(window, 1);
        }
    }
}
