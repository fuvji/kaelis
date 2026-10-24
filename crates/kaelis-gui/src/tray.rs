//! Native tray icon and event bridge. The receiver blocks while idle, so it
//! does not poll while the launcher is hidden.

use crate::GuiEvent;
use eframe::egui;
use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub(crate) struct TrayController {
    _icon: TrayIcon,
}

impl TrayController {
    pub(crate) fn install(
        context: egui::Context,
        events: UnboundedSender<GuiEvent>,
    ) -> Result<Self, String> {
        let menu = Menu::new();
        let show = MenuItem::new("Show Kaelis", true, None);
        let launch = MenuItem::new("Launch last instance", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[&show, &launch, &quit])
            .map_err(|error| error.to_string())?;

        let show_id = show.id().clone();
        let launch_id = launch.id().clone();
        let quit_id = quit.id().clone();
        let receiver = MenuEvent::receiver().clone();
        let click_events = events.clone();
        let click_context = context.clone();
        std::thread::Builder::new()
            .name("kaelis-tray-events".to_owned())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    let action = if event.id == show_id {
                        Some(TrayAction::Show)
                    } else if event.id == launch_id {
                        Some(TrayAction::LaunchLast)
                    } else if event.id == quit_id {
                        Some(TrayAction::Quit)
                    } else {
                        None
                    };
                    if let Some(action) = action {
                        // The window may be hidden for a running game, and a
                        // hidden window paints no frame — so the event alone
                        // would never be processed. Wake the real window first
                        // from this thread; the event then syncs the toolkit.
                        // Quit needs the same wake: Close is a viewport command
                        // and a hidden viewport would never apply it.
                        crate::platform::wake(&context);
                        let _ = events.send(GuiEvent::Tray(action));
                        context.request_repaint();
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        // A tray icon is also a button. Clicking it is the gesture every other
        // tray application answers, so it restores the window instead of
        // silently doing nothing and leaving the menu as the only way back.
        std::thread::Builder::new()
            .name("kaelis-tray-click".to_owned())
            .spawn(move || {
                let receiver = TrayIconEvent::receiver().clone();
                while let Ok(event) = receiver.recv() {
                    let restores = matches!(
                        event,
                        TrayIconEvent::DoubleClick { .. }
                            | TrayIconEvent::Click {
                                button: MouseButton::Left,
                                button_state: MouseButtonState::Up,
                                ..
                            }
                    );
                    if restores {
                        crate::platform::wake(&click_context);
                        let _ = click_events.send(GuiEvent::Tray(TrayAction::Show));
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        let icon = brand_icon().map_err(|error| error.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("Kaelis Minecraft Launcher")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            // Left click is the direct gesture; the menu stays on right click.
            // Without this, Windows turns a left click into the menu and the
            // icon never emits a Click event at all.
            .with_menu_on_left_click(false)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { _icon: tray })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TrayAction {
    Show,
    LaunchLast,
    Quit,
}

/// Same geometry as the window icon, so the tray and taskbar never show two
/// different logos.
fn brand_icon() -> Result<Icon, tray_icon::BadIcon> {
    let data = crate::mark::primary_icon(32);
    Icon::from_rgba(data.rgba, data.width, data.height)
}
