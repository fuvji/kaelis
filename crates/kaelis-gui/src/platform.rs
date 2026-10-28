//! Native window handling that egui cannot do by itself.
//!
//! egui applies `ViewportCommand`s only while it is painting a frame, and a
//! hidden window may never paint again. Kaelis therefore never asks egui to
//! hide its viewport: on Windows the real HWND is hidden with Win32 and shown
//! with Win32, so the toolkit's idea of the window never goes stale. Every
//! restore also raises the window above a running game and asks for one paint,
//! which is what makes the tray item work while Minecraft owns the screen.

use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the launcher window is on screen. Kept in sync by every native
/// show/hide path, including the taskbar's minimize button, so the tray toggle
/// never has to enumerate windows just to answer one question.
static WINDOW_SHOWN: AtomicBool = AtomicBool::new(true);

/// Set while a sign-in launch is trying to stay invisible; any user restore
/// clears it so the startup watcher cannot hide the window again.
static STARTUP_SUPPRESS: AtomicBool = AtomicBool::new(false);

/// Windows' background-mode latch: one atomic load instead of two syscalls.
#[cfg(windows)]
static IDLE_MODE: AtomicBool = AtomicBool::new(false);

/// Drop this process to the bottom of the system, or bring it back.
///
/// Every call here is a documented Windows power API, and each is reversed by
/// its twin:
///
/// * **Background processing mode** (`SetPriorityClass(PROCESS_MODE_BACKGROUND_BEGIN)`)
///   moves every thread to the lowest usable CPU priority, sets the process's
///   disk and memory priorities to their minimums, and disables priority
///   boosts. The launcher stops being scheduled ahead of anything the user is
///   doing, and its disk work stops competing with the game.
/// * **EcoQoS** (`SetProcessInformation(ProcessPowerThrottling, …EXECUTION_SPEED)`)
///   lets the scheduler slow the process down when the power budget is tight —
///   on a hybrid CPU that means efficiency cores at a low clock instead of a
///   performance core at full speed.
///
/// Together with the working-set trim this is as close to "not running" as a
/// process can get while still existing: the pages are paged out, and what is
/// left cannot take a core from anybody. Both calls are advisory — a host that
/// refuses them (an outer job object owning the priority, for example) keeps a
/// normally scheduled launcher rather than a broken one.
pub(crate) fn set_idle_mode(idle: bool) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN, PROCESS_MODE_BACKGROUND_END,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass,
            SetProcessInformation,
        };
        if IDLE_MODE.swap(idle, Ordering::Relaxed) == idle {
            return;
        }
        // SAFETY: the pseudo-handle is always valid; the throttling structure is
        // a plain `#[repr(C)]` value for the documented class, and the size
        // passed is the size of that structure.
        unsafe {
            let process = GetCurrentProcess();
            SetPriorityClass(
                process,
                if idle {
                    PROCESS_MODE_BACKGROUND_BEGIN
                } else {
                    PROCESS_MODE_BACKGROUND_END
                },
            );
            let throttle = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                StateMask: if idle {
                    PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                } else {
                    0
                },
            };
            SetProcessInformation(
                process,
                ProcessPowerThrottling,
                std::ptr::from_ref(&throttle).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = idle;
    }
}

/// Whether the launcher's window is hidden right now.
///
/// Every hide and restore path maintains the flag this reads, so the per-frame
/// check is one atomic load instead of a window enumeration; the native check
/// stays in [`window_is_shown`], where a tray click needs the literal truth.
pub(crate) fn window_is_hidden() -> bool {
    !WINDOW_SHOWN.load(Ordering::Relaxed)
}

pub(crate) fn window_is_shown() -> bool {
    // On Windows the real handle is the truth, including a window the shell
    // hid without a minimize command. The atomic covers toolkits where the
    // window is merely an invisible viewport.
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};
        if let Some(window) = main_window() {
            // A minimized window is still "visible" to IsWindowVisible, but a
            // tray click must restore it rather than hide it. Treat the
            // taskbar-minimized state as not shown so the click wakes it.
            // SAFETY: the handle belongs to this process.
            return unsafe { IsWindowVisible(window) } != 0 && unsafe { IsIconic(window) } == 0;
        }
    }
    WINDOW_SHOWN.load(Ordering::Relaxed)
}

/// Keep a sign-in launch invisible.
///
/// eframe deliberately shows the window after its first painted frame (the
/// white-flash fix), so a hidden viewport alone becomes visible a moment later.
/// This waits for that show *event* and hides the native window in the same
/// millisecond, because a sampler would either hide it late or wake the process
/// for nothing. `wake` cancels the watcher, so a tray click during start-up is
/// never undone.
#[cfg(windows)]
pub(crate) fn begin_hidden_start(context: egui::Context) {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_SHOW, GetMessageW, MSG, WINEVENT_OUTOFCONTEXT,
    };

    static CONTEXT: OnceLock<egui::Context> = OnceLock::new();
    static HIDDEN: AtomicBool = AtomicBool::new(false);

    /// The launcher's window became visible: undo it. Only the first show
    /// matters, and only while the sign-in launch still wants to stay hidden.
    unsafe extern "system" fn shown(
        _hook: HWINEVENTHOOK,
        _event: u32,
        _window: windows_sys::Win32::Foundation::HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if HIDDEN.swap(true, Ordering::SeqCst) || !STARTUP_SUPPRESS.load(Ordering::SeqCst) {
            return;
        }
        if let Some(context) = CONTEXT.get() {
            hide_for_game(context);
        }
    }

    STARTUP_SUPPRESS.store(true, Ordering::SeqCst);
    HIDDEN.store(false, Ordering::SeqCst);
    // A sign-in launch is invisible from the first instruction, so it waits for
    // the user at the bottom of the schedule: nothing it does is worth taking a
    // core from the desktop. A tray click reverses this before the window is
    // shown (`wake`).
    set_idle_mode(true);
    let _ = CONTEXT.set(context);
    let _ = std::thread::Builder::new()
        .name("kaelis-hidden-start".to_owned())
        .spawn(|| {
            // SAFETY: the hook is process-wide and filtered to this process,
            // the callback is a plain `fn` that touches only atomics and the
            // stored context, and no module handle is needed for an
            // out-of-context hook.
            let pid = unsafe { GetCurrentProcessId() };
            let hook = unsafe {
                SetWinEventHook(
                    EVENT_OBJECT_SHOW,
                    EVENT_OBJECT_SHOW,
                    std::ptr::null_mut(),
                    Some(shown),
                    pid,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            if hook.is_null() {
                STARTUP_SUPPRESS.store(false, Ordering::SeqCst);
                return;
            }
            let mut message: MSG = unsafe { std::mem::zeroed() };
            // Out-of-context events arrive as messages on this thread's queue,
            // so it has to pump them. The loop ends on the first show, or with
            // the process — whichever comes first.
            while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
                if HIDDEN.load(Ordering::SeqCst) {
                    break;
                }
            }
            // SAFETY: `hook` came from SetWinEventHook and has not been removed.
            unsafe { UnhookWinEvent(hook) };
        });
}

#[cfg(not(windows))]
pub(crate) fn begin_hidden_start(context: egui::Context) {
    let _ = context;
}

/// Dark native menus.
///
/// Windows draws its own tray menu, and the documented theming APIs do not
/// cover popup menus. The two undocumented `uxtheme` ordinals below are what
/// every dark Windows application uses: 135 selects the process-wide app mode
/// (`2` = force dark) and 136 flushes cached menu themes so a menu created
/// afterwards is painted dark. Both are optional at runtime — on a build that
/// does not export them the call is a silent no-op and the menu keeps the
/// system theme instead of breaking.
pub(crate) fn enable_dark_menus() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

        type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
        type FlushMenuThemes = unsafe extern "system" fn();

        // SAFETY: uxtheme.dll is already loaded in every process that draws
        // windows. Ordinals are resolved by number because the exports are
        // undocumented; a missing export yields a null pointer that is checked
        // before use.
        unsafe {
            let module = GetModuleHandleW(wide("uxtheme.dll").as_ptr());
            if module.is_null() {
                return;
            }
            let set_mode = std::mem::transmute::<
                windows_sys::Win32::Foundation::FARPROC,
                Option<SetPreferredAppMode>,
            >(GetProcAddress(module, 135 as *const u8));
            if let Some(set_mode) = set_mode {
                set_mode(2);
            }
            let flush = std::mem::transmute::<
                windows_sys::Win32::Foundation::FARPROC,
                Option<FlushMenuThemes>,
            >(GetProcAddress(module, 136 as *const u8));
            if let Some(flush) = flush {
                flush();
            }
        }
    }
}

/// Route the title-bar minimize button to the tray.
///
/// egui only learns about a minimize after the toolkit has already minimized
/// the window, which means a repaint (and a visible animation) before the
/// launcher could hide itself. A window subclass sees `WM_SYSCOMMAND` before
/// `DefWindowProc` and hides the real handle instead, so the button costs one
/// message and no frame. `SetWindowSubclass` chains to the toolkit's own
/// procedure, so restore, resize, and painting keep working unchanged.
#[cfg(windows)]
pub(crate) fn install_window_hooks() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SC_MINIMIZE, WM_SYSCOMMAND};

    static INSTALLED: AtomicBool = AtomicBool::new(false);

    if INSTALLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(window) = main_window() else {
        // The window may not exist during the first construction step; the
        // next frame retries because the flag stays false.
        return;
    };

    unsafe extern "system" fn minimized(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if message == WM_SYSCOMMAND && (wparam as u32 & 0xFFF0) == SC_MINIMIZE {
            hide_window(window);
            WINDOW_SHOWN.store(false, Ordering::Relaxed);
            set_idle_mode(true);
            return 0;
        }
        // SAFETY: the same arguments arrived from the window manager; the
        // default procedure owns every message this hook does not consume.
        unsafe { DefSubclassProc(window, message, wparam, lparam) }
    }

    // SAFETY: the callback has the documented `SUBCLASSPROC` signature, the
    // window belongs to this process, and the id is private to Kaelis.
    let installed = unsafe { SetWindowSubclass(window, Some(minimized), 0x4b41_454c, 0) } != 0;
    INSTALLED.store(installed, Ordering::Relaxed);
}

#[cfg(not(windows))]
pub(crate) fn install_window_hooks() {}

/// Register or remove the per-user sign-in entry.
///
/// A value under `HKCU\…\Run` is the lightest possible startup hook: Windows
/// starts the executable directly, with no service, scheduled task, helper
/// process, or delayed launch. `--tray` keeps the window hidden so signing in
/// never flashes a launcher over the desktop.
#[cfg(windows)]
pub(crate) fn set_startup(enabled: bool) -> Result<(), String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
    };

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE_NAME: &str = "Kaelis";

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let command = format!("\"{}\" --tray", executable.display());
    let subkey = wide(RUN_KEY);
    let value_name = wide(VALUE_NAME);
    let mut data: Vec<u16> = command.encode_utf16().collect();
    data.push(0);

    let mut key: HKEY = std::ptr::null_mut();
    let mut disposition = 0_u32;
    // SAFETY: both strings are NUL terminated and the out-pointers are valid.
    let opened = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            &mut disposition,
        )
    };
    if opened != ERROR_SUCCESS {
        return Err(format!("could not open the startup key ({opened})"));
    }

    let result = if enabled {
        // SAFETY: `key` is an open writable key and `data` outlives the call.
        let written = unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast::<u8>(),
                (data.len() * std::mem::size_of::<u16>()) as u32,
            )
        };
        if written == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("could not write the startup entry ({written})"))
        }
    } else {
        // A missing value already means "off"; its absence is the goal.
        // SAFETY: the key is open and the name is NUL terminated.
        unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
        Ok(())
    };

    // SAFETY: the key came from RegCreateKeyExW and is owned here.
    unsafe { RegCloseKey(key) };
    result
}

#[cfg(not(windows))]
pub(crate) fn set_startup(_enabled: bool) -> Result<(), String> {
    Err("starting with the system is available on Windows only".to_owned())
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

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
        let _ = context;
        if let Some(window) = main_window() {
            hide_window(window);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = context;
        context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
    WINDOW_SHOWN.store(false, Ordering::Relaxed);
    // Nothing is visible and nothing is being asked of this process, so it goes
    // to the bottom of the schedule until it is wanted again.
    set_idle_mode(true);
}

/// Take the launcher's window off the screen without waking the event loop.
///
/// Minimizing first is not cosmetic, it is what keeps a hidden launcher from
/// burning a core. The toolkit checks the *iconic* state before scheduling
/// another paint, and an invisible window can never deliver the paint it would
/// be waiting for — so a window that is only hidden leaves that paint pending
/// and the event loop spins at full speed on a window nobody can see. (That is
/// the whole reason a hidden launcher used to sit at 5–13% of a core.) Hiding
/// immediately afterwards still takes the taskbar button away, so the tray
/// icon remains the only handle, and `IsIconic` stays true, which is what the
/// scheduling check reads.
#[cfg(windows)]
fn hide_window(window: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_MINIMIZE, ShowWindow};
    // SAFETY: the handle belongs to this process; both calls only change the
    // window's own show state.
    unsafe {
        ShowWindow(window, SW_MINIMIZE);
        ShowWindow(window, SW_HIDE);
    }
}

/// The tray's Show / Minimize action: hide a visible launcher and give its
/// pages back, or restore a hidden one. One entry point so the menu never
/// disagrees with the window state.
pub(crate) fn hide_to_tray(context: &egui::Context) {
    hide_for_game(context);
    trim_working_set();
}

/// Ask the compositor for smoothly rounded corners and no window border.
///
/// The launcher paints its own header, so it has no frame for the system to
/// round. Windows 11 rounds a resizable window at composition time when asked,
/// which costs the launcher nothing per frame — no transparent surface, no
/// per-pixel alpha, no extra draw call. Windows 10 ignores the request and
/// keeps its own default, so a failed call is not an error.
///
/// Returns `true` once the launcher's window was found and the request made,
/// so a caller can retry while the handle does not exist yet and stop after it
/// does — including on a system that answers with an error.
#[cfg(windows)]
pub(crate) fn round_window_corners() -> bool {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    /// `DWMWA_WINDOW_CORNER_PREFERENCE`.
    const CORNER_PREFERENCE: u32 = 33;
    /// `DWMWA_BORDER_COLOR`.
    const BORDER_COLOR: u32 = 34;
    /// `DWMWCP_ROUND`: the rounded-rectangle preference.
    const ROUND: i32 = 2;
    /// `DWMWA_COLOR_NONE`: draw no border at all.
    const NO_BORDER: u32 = 0xFFFF_FFFE;

    let Some(window) = main_window() else {
        return false;
    };
    // SAFETY: `window` belongs to this process, and both pointers are valid
    // for the four bytes the attribute expects.
    unsafe {
        DwmSetWindowAttribute(
            window,
            CORNER_PREFERENCE,
            std::ptr::from_ref(&ROUND).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        DwmSetWindowAttribute(
            window,
            BORDER_COLOR,
            std::ptr::from_ref(&NO_BORDER).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
    true
}

/// The header's minimize control on an overlay with no native title bar.
///
/// On Windows the launcher's own behaviour is the tray, so the custom button
/// does exactly what the old title-bar button did. Elsewhere it is an ordinary
/// minimize, so the window can be restored from the taskbar or the dock.
pub(crate) fn minimize_window(context: &egui::Context) {
    #[cfg(windows)]
    {
        hide_to_tray(context);
    }
    #[cfg(not(windows))]
    {
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
    // Ahead of everything else: a process in background-mode and EcoQoS is
    // served last, and the restore itself is the one thing that must not be.
    set_idle_mode(false);
    STARTUP_SUPPRESS.store(false, Ordering::Relaxed);
    #[cfg(windows)]
    restore_windows_window();
    WINDOW_SHOWN.store(true, Ordering::Relaxed);
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
    set_idle_mode(false);
    STARTUP_SUPPRESS.store(false, Ordering::Relaxed);
    #[cfg(windows)]
    restore_windows_window();
    WINDOW_SHOWN.store(true, Ordering::Relaxed);
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
    ) -> windows_sys::core::BOOL {
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
