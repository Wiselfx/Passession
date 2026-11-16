//! macOS and Windows status-bar / notification-area icon (original tray_manager):
//! tooltip "Passession", menu Open Passession / separator / Quit Passession in the
//! app's language, headed by the embedded Tailscale status, Pause or Resume and
//! Stop while it is enabled. A left click or double click opens the preferred
//! window. Created on the GUI (main) thread, whose run loop / message loop
//! services it.

use super::{NativeSignal, TrayMenu, deliver};
use crate::{
    action::{TailnetControl, TailnetPresenceState},
    platform::Shared,
};
use std::sync::{Arc, atomic::Ordering};
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};

/// Fixed item ids: the menu is rebuilt on every label or presence change, while
/// the process-wide event handler is installed once.
const OPEN: &str = "sh.paseo.tray.open";
const QUIT: &str = "sh.paseo.tray.quit";
const PAUSE: &str = "sh.paseo.tray.tailnet.pause";
const RESUME: &str = "sh.paseo.tray.tailnet.resume";
const STOP: &str = "sh.paseo.tray.tailnet.stop";

pub(crate) struct Tray {
    icon: tray_icon::TrayIcon,
    shared: Arc<Shared>,
}

impl Tray {
    pub fn poll(&mut self) {}

    /// muda items cannot be hidden, so the whole menu is replaced.
    pub fn set_menu(&mut self, menu: &TrayMenu) {
        match build_menu(menu) {
            Ok(menu) => self.icon.set_menu(Some(Box::new(menu))),
            Err(message) => self.shared.desktop.report(&message),
        }
    }
}

pub(crate) fn create_tray(shared: &Arc<Shared>) {
    let owner = shared.clone();
    shared.on_main(move |_, state| match build_tray(&owner) {
        Ok(tray) => {
            if !owner.desktop.quitting() {
                state.desktop.install_tray(tray);
                owner.desktop.tray_created.store(true, Ordering::SeqCst);
            }
        }
        Err(message) => owner.desktop.report(&message),
    });
}

/// Localized labels are literal text: `&` would otherwise mark a mnemonic.
fn literal(text: &str) -> String {
    text.replace('&', "&&")
}

fn build_menu(content: &TrayMenu) -> Result<Menu, String> {
    let menu = Menu::new();
    let append = |item: &dyn IsMenuItem| menu.append(item).map_err(|e| e.to_string());
    if let Some(presence) = &content.tailnet {
        let status = format!("{}: {}", presence.title, presence.status);
        append(&MenuItem::new(literal(&status), false, None))?;
        let (id, label) = match presence.state {
            TailnetPresenceState::Paused => (RESUME, &presence.resume),
            _ => (PAUSE, &presence.pause),
        };
        append(&MenuItem::with_id(id, literal(label), true, None))?;
        append(&MenuItem::with_id(
            STOP,
            literal(&presence.stop),
            true,
            None,
        ))?;
        append(&PredefinedMenuItem::separator())?;
    }
    append(&MenuItem::with_id(OPEN, literal(&content.open), true, None))?;
    append(&PredefinedMenuItem::separator())?;
    append(&MenuItem::with_id(QUIT, literal(&content.quit), true, None))?;
    Ok(menu)
}

fn build_tray(shared: &Arc<Shared>) -> Result<Tray, String> {
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    // macOS draws the menu-bar icon as a template (alpha only, recoloured).
    #[cfg(target_os = "macos")]
    let bytes = &include_bytes!("../../assets/trayTemplate.png")[..];
    #[cfg(not(target_os = "macos"))]
    let bytes = &include_bytes!("../../assets/tray.png")[..];
    let (width, height, rgba) = crate::image::decode_png_rgba(bytes)
        .map_err(|e| format!("Unable to load tray icon: {e}"))?;
    let icon = tray_icon::Icon::from_rgba(rgba, width, height)
        .map_err(|e| format!("Unable to load tray icon: {e}"))?;
    let menu = build_menu(&TrayMenu::default())?;
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        let signal = match event.id.0.as_str() {
            OPEN => NativeSignal::TrayOpen,
            QUIT => NativeSignal::TrayQuit,
            PAUSE => NativeSignal::TrayTailnet(TailnetControl::Pause),
            RESUME => NativeSignal::TrayTailnet(TailnetControl::Resume),
            STOP => NativeSignal::TrayTailnet(TailnetControl::Stop),
            _ => return,
        };
        deliver(signal);
    }));
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
        | TrayIconEvent::DoubleClick { .. } => deliver(NativeSignal::TrayOpen),
        _ => {}
    }));
    let builder = TrayIconBuilder::new()
        .with_id(crate::paths::DESKTOP_APP_ID)
        .with_tooltip("Passession");
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon);
    let icon = builder
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| format!("Unable to create the system tray icon: {e}"))?;
    Ok(Tray {
        icon,
        shared: shared.clone(),
    })
}

/// macOS and Windows always host status/notification-area icons.
pub(crate) async fn tray_host_available() -> bool {
    true
}
