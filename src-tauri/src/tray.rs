//! The tray icon: two states (idle/fitted), a tooltip that names the hotkey or the fitted
//! window, left-click opens the window, menu Open / Fit-or-Restore / Exit.
//!
//! Tray methods block until the main thread services them, so updates are always posted to
//! the main thread with `run_on_main_thread` and never called from the engine thread.

use crate::engine::{FitSource, Msg};
use crate::model::{FitState, FitStatus};
use crate::state::{self, lock, Core};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "main";
const MENU_OPEN: &str = "open";
const MENU_TOGGLE: &str = "toggle";
const MENU_EXIT: &str = "exit";
/// Windows truncates tray tooltips at 127 characters.
const TOOLTIP_MAX: usize = 120;
const FAILURE_TOOLTIP: &str = "BorderFit · couldn't fit, open for details";

/// Menu items whose labels change with the status.
struct TrayItems {
    toggle: MenuItem<Wry>,
}

fn idle_icon() -> Image<'static> {
    tauri::include_image!("icons/tray-idle.png")
}

fn fitted_icon() -> Image<'static> {
    tauri::include_image!("icons/tray-fitted.png")
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, MENU_OPEN, "Open BorderFit", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, MENU_TOGGLE, "Fit active window", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, MENU_EXIT, "Exit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &toggle, &separator, &exit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(idle_icon())
        .tooltip("BorderFit")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN => show_main(app),
            MENU_TOGGLE => {
                if let Some(core) = app.try_state::<Core>() {
                    core.send(Msg::Toggle { src: FitSource::Tray });
                }
            }
            MENU_EXIT => state::quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    app.manage(TrayItems { toggle });
    refresh(app);
    Ok(())
}

/// Shows, un-minimizes and focuses the main window.
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Re-applies icon, tooltip and menu label from the current status (posted to the main thread).
pub fn refresh(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || apply(&handle, None));
}

/// Shows the "couldn't fit" tooltip for 5 seconds.
pub fn flash_failure(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || apply(&handle, Some(FAILURE_TOOLTIP)));
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        refresh(&handle);
    });
}

/// Main thread only.
fn apply(app: &AppHandle, tooltip_override: Option<&str>) {
    let Some(core) = app.try_state::<Core>() else { return };
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let status = lock(&core.last_status).clone();
    let chord = lock(&core.hotkey).chord.clone();
    let fitted = status.state == FitState::Fitted;

    let _ = tray.set_icon(Some(if fitted { fitted_icon() } else { idle_icon() }));
    let tooltip = tooltip_override.map_or_else(|| tooltip_for(&status, &chord), str::to_string);
    let _ = tray.set_tooltip(Some(truncate(&tooltip, TOOLTIP_MAX)));
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.toggle.set_text(toggle_label(&status));
    }
}

fn window_title(status: &FitStatus) -> String {
    status.window.as_ref().map_or_else(String::new, |w| {
        if w.title.is_empty() {
            w.exe.clone()
        } else {
            w.title.clone()
        }
    })
}

fn tooltip_for(status: &FitStatus, chord: &str) -> String {
    match status.state {
        FitState::Fitted => format!("BorderFit · fitted: {}", window_title(status)),
        FitState::Idle => format!("BorderFit · {chord} to fit"),
    }
}

fn toggle_label(status: &FitStatus) -> String {
    match status.state {
        FitState::Fitted => truncate(&format!("Restore \"{}\"", window_title(status)), 60),
        FitState::Idle => "Fit active window".to_string(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowSummary;

    #[test]
    fn labels_follow_the_status() {
        let idle = FitStatus::default();
        assert_eq!(tooltip_for(&idle, "Ctrl+Alt+B"), "BorderFit · Ctrl+Alt+B to fit");
        assert_eq!(toggle_label(&idle), "Fit active window");
        let fitted = FitStatus {
            state: FitState::Fitted,
            window: Some(WindowSummary {
                hwnd: 1,
                pid: 2,
                exe: "eldenring.exe".into(),
                title: "ELDEN RING".into(),
            }),
            ..FitStatus::default()
        };
        assert_eq!(tooltip_for(&fitted, "F12"), "BorderFit · fitted: ELDEN RING");
        assert_eq!(toggle_label(&fitted), "Restore \"ELDEN RING\"");
    }

    #[test]
    fn long_titles_are_truncated() {
        let long = "x".repeat(200);
        assert_eq!(truncate(&long, 10).chars().count(), 10);
        assert!(truncate(&long, 10).ends_with('…'));
    }
}
