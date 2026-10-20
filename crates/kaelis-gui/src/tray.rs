//! Native tray icon and event bridge. The receiver blocks while idle, so it
//! does not poll while the launcher is hidden.

use crate::GuiEvent;
use eframe::egui;
use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

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
                        let _ = events.send(GuiEvent::Tray(action));
                        context.request_repaint();
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        let icon = brand_icon().map_err(|error| error.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("Kaelis Minecraft Launcher")
            .with_icon(icon)
            .with_menu(Box::new(menu))
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

fn brand_icon() -> Result<Icon, tray_icon::BadIcon> {
    const SIZE: usize = 64;
    let mut rgba = vec![0_u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inset = (8..56).contains(&x) && (8..56).contains(&y);
            let diagonal =
                (x as isize - y as isize).unsigned_abs() <= 5 || (x + y).abs_diff(63) <= 5;
            if inset {
                let index = (y * SIZE + x) * 4;
                let accent = if diagonal {
                    [120, 199, 255, 255]
                } else {
                    [26, 34, 52, 255]
                };
                rgba[index..index + 4].copy_from_slice(&accent);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE as u32, SIZE as u32)
}
